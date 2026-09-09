//! `m20260909_000001_code_pr_delivery` — per-session pull-request delivery.
//!
//! Two tables make delivery durable and replay-safe without a global
//! suppression marker:
//!
//! - `code_pr_delivery_state` holds, per `(session, pull-request, family)`,
//!   the state token of the last event we planned. Repeated observations of
//!   the same fact no-op; a re-entrant transition (pending after failing,
//!   changes-requested after approved, blocked after watching) advances the
//!   token and mints a new occurrence.
//! - `code_pr_delivery_outbox` holds the journal payloads that have not yet
//!   been appended. The journal append and the `delivered` write commit in
//!   one transaction, so a restart neither loses an event nor appends it
//!   twice. A failed append leaves the row queued and a sweep retries it.
//!
//! The outbox is keyed per session: one session's transient failure never
//! marks another session's delivery as done.

use sea_orm_migration::prelude::*;

use super::idens::{CodePrDeliveryOutbox, CodePrDeliveryState};

pub(super) struct PrDeliveryState;

impl MigrationName for PrDeliveryState {
    fn name(&self) -> &str {
        "m20260909_000001_code_pr_delivery"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for PrDeliveryState {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(CodePrDeliveryState::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(CodePrDeliveryState::Owner).text().not_null())
                    .col(
                        ColumnDef::new(CodePrDeliveryState::SessionId)
                            .uuid()
                            .not_null(),
                    )
                    .col(ColumnDef::new(CodePrDeliveryState::Host).text().not_null())
                    .col(
                        ColumnDef::new(CodePrDeliveryState::RepoOwner)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(CodePrDeliveryState::RepoName)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(CodePrDeliveryState::Number)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(CodePrDeliveryState::Family)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(CodePrDeliveryState::LastStateToken)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(CodePrDeliveryState::NextOccurrence)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(CodePrDeliveryState::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .primary_key(
                        Index::create()
                            .col(CodePrDeliveryState::SessionId)
                            .col(CodePrDeliveryState::Family),
                    )
                    .check(Expr::col(CodePrDeliveryState::Number).gte(1))
                    .check(Expr::col(CodePrDeliveryState::NextOccurrence).gte(1))
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("ix_code_pr_delivery_state_pr")
                    .table(CodePrDeliveryState::Table)
                    .col(CodePrDeliveryState::Owner)
                    .col(CodePrDeliveryState::Host)
                    .col(CodePrDeliveryState::RepoOwner)
                    .col(CodePrDeliveryState::RepoName)
                    .col(CodePrDeliveryState::Number)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .create_table(
                Table::create()
                    .table(CodePrDeliveryOutbox::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(CodePrDeliveryOutbox::Owner)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(CodePrDeliveryOutbox::SessionId)
                            .uuid()
                            .not_null(),
                    )
                    .col(ColumnDef::new(CodePrDeliveryOutbox::Host).text().not_null())
                    .col(
                        ColumnDef::new(CodePrDeliveryOutbox::RepoOwner)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(CodePrDeliveryOutbox::RepoName)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(CodePrDeliveryOutbox::Number)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(CodePrDeliveryOutbox::Family)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(CodePrDeliveryOutbox::Occurrence)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(CodePrDeliveryOutbox::EventJson)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(CodePrDeliveryOutbox::QueuedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(CodePrDeliveryOutbox::DeliveredAt)
                            .timestamp_with_time_zone(),
                    )
                    .col(ColumnDef::new(CodePrDeliveryOutbox::DeliveredSeq).big_integer())
                    .primary_key(
                        Index::create()
                            .col(CodePrDeliveryOutbox::SessionId)
                            .col(CodePrDeliveryOutbox::Family)
                            .col(CodePrDeliveryOutbox::Occurrence),
                    )
                    .check(Expr::col(CodePrDeliveryOutbox::Number).gte(1))
                    .check(Expr::col(CodePrDeliveryOutbox::Occurrence).gte(1))
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("ix_code_pr_delivery_outbox_pending")
                    .table(CodePrDeliveryOutbox::Table)
                    .col(CodePrDeliveryOutbox::DeliveredAt)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Keep delivery state and outbox on rollback: discarding either
        // could re-render or drop a fact the adapter already saw.
        Ok(())
    }
}
