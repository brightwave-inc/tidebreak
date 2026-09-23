//! `m20260923_000002_memory_evidence_event_kind`: spell code-session memory
//! evidence `event`.
//!
//! The shared-noun rename changed how memory evidence from a code session is
//! spelled, from `code_event` to `event`, without rewriting what was already
//! saved. Those records then failed to load, and one of them was enough to
//! fail the whole memory list. This rewrites the old spelling in saved records
//! and in their revision snapshots. Both columns hold compact JSON on SQLite;
//! PostgreSQL renders `jsonb` as text with a space after each colon.

use sea_orm::{ConnectionTrait, DbBackend};
use sea_orm_migration::prelude::*;

pub(super) struct MemoryEvidenceEventKind;

impl MigrationName for MemoryEvidenceEventKind {
    fn name(&self) -> &str {
        "m20260923_000002_memory_evidence_event_kind"
    }
}

/// Each JSON column that carries a record's provenance.
const COLUMNS: [(&str, &str); 2] = [
    ("memory_record", "provenance"),
    ("memory_revision", "snapshot"),
];

#[async_trait::async_trait]
impl MigrationTrait for MemoryEvidenceEventKind {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let postgres = manager.get_database_backend() == DbBackend::Postgres;
        for (table, column) in COLUMNS {
            if !manager.has_table(table).await? {
                continue;
            }
            let sql = if postgres {
                format!(
                    r#"UPDATE "{table}" SET "{column}" = replace("{column}"::text, '"kind": "code_event"', '"kind": "event"')::jsonb WHERE "{column}"::text LIKE '%"kind": "code_event"%'"#
                )
            } else {
                format!(
                    r#"UPDATE "{table}" SET "{column}" = replace("{column}", '"kind":"code_event"', '"kind":"event"') WHERE "{column}" LIKE '%"kind":"code_event"%'"#
                )
            };
            manager.get_connection().execute_unprepared(&sql).await?;
        }
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // `event` is the only spelling this build writes, and it still reads
        // `code_event`, so there is nothing to put back.
        Ok(())
    }
}
