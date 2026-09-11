//! Durable admission metadata for external slack steering
//! (`docs/slack-sessions.md`, steer outcome).
//!
//! One external channel event already commits with its queue row; this
//! migration records whether that delivery asked to steer, the native turn
//! it was admitted against, the caller correlation id, and the machine's
//! durable resolution (`steered` or `queued` with a reason). `outcome` stays
//! null until the admission resolves, so a replay can answer honestly while
//! the engine is still acknowledging steering.
use sea_orm_migration::prelude::*;

use super::idens::CodeExternalEvent;

pub(super) struct ExternalSteerAdmission;

impl MigrationName for ExternalSteerAdmission {
    fn name(&self) -> &str {
        "m20260911_000001_external_steer_admission"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for ExternalSteerAdmission {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for column in [
            CodeExternalEvent::SteerRequested,
            CodeExternalEvent::ExpectedTurnId,
            CodeExternalEvent::CorrelationUuid,
            CodeExternalEvent::Outcome,
            CodeExternalEvent::OutcomeReason,
            CodeExternalEvent::OutcomeAt,
        ] {
            let name = column.to_string();
            if manager.has_column("code_external_event", &name).await? {
                continue;
            }
            let column_def = {
                let mut def = ColumnDef::new(column);
                match name.as_str() {
                    "steer_requested" => {
                        def.boolean().not_null().default(false);
                    }
                    "expected_turn_id" | "correlation_uuid" => {
                        def.uuid();
                    }
                    "outcome" | "outcome_reason" => {
                        def.text();
                    }
                    "outcome_at" => {
                        def.timestamp_with_time_zone();
                    }
                    _ => unreachable!("all new columns are handled"),
                }
                def
            };
            manager
                .alter_table(
                    Table::alter()
                        .table(CodeExternalEvent::Table)
                        .add_column_if_not_exists(column_def)
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Dropping the columns would forget admitted steering and replay
        // safety; a rolled-back release leaves harmless nullable columns.
        Ok(())
    }
}
