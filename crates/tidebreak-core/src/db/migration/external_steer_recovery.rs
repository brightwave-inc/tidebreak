//! Explicit recovery decisions remain separate from native steering admission.
use sea_orm_migration::prelude::*;

use super::idens::CodeExternalEvent;

pub(super) struct ExternalSteerRecovery;

impl MigrationName for ExternalSteerRecovery {
    fn name(&self) -> &str {
        "m20260911_000002_external_steer_recovery"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for ExternalSteerRecovery {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for column in [
            CodeExternalEvent::RecoveryAction,
            CodeExternalEvent::RecoveryRetryTurnId,
            CodeExternalEvent::RecoveredAt,
        ] {
            let name = column.to_string();
            if manager.has_column("code_external_event", &name).await? {
                continue;
            }
            let mut definition = ColumnDef::new(column);
            match name.as_str() {
                "recovery_action" => {
                    definition.text();
                }
                "recovery_retry_turn_id" => {
                    definition.uuid();
                }
                "recovered_at" => {
                    definition.timestamp_with_time_zone();
                }
                _ => unreachable!("all recovery columns are handled"),
            }
            manager
                .alter_table(
                    Table::alter()
                        .table(CodeExternalEvent::Table)
                        .add_column_if_not_exists(definition)
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Retain recovery records so rolling back cannot create another retry.
        Ok(())
    }
}
