//! The ordered progress stream a background run publishes while it works.
//!
//! This is the one part of the agent-run tier that is not correctness state.
//! Nothing reads it to decide what a run may do next, so the append does not
//! take a lease, does not fence, and is deliberately allowed to fail without
//! disturbing the transition that produced the line. What it does guarantee is
//! ordering and idempotency: a per-run sequence a reader can resume from, and a
//! producer-supplied `source_key` that makes redelivering a line a no-op.

use chrono::Utc;
use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, Set,
    TransactionTrait,
};

use crate::error::Result;
use crate::id::AgentRunId;
use crate::model::AgentRunProgressEntry;

use super::super::super::{entities, store_err, DbStore};

/// How many times to retry a sequence assignment that lost a race. Only one
/// producer publishes a run's progress at a time, so this is defence against an
/// overlapping handoff rather than an expected path.
const ASSIGNMENT_ATTEMPTS: u32 = 3;

pub(in crate::db) async fn append(
    store: &DbStore,
    run_id: AgentRunId,
    source_key: &str,
    text: &str,
) -> Result<()> {
    let text = truncate_on_char_boundary(text.trim(), AgentRunProgressEntry::MAX_TEXT_LEN);
    if text.is_empty() {
        return Ok(());
    }
    let source_key =
        truncate_on_char_boundary(source_key, AgentRunProgressEntry::MAX_SOURCE_KEY_LEN);

    for _ in 0..ASSIGNMENT_ATTEMPTS {
        let transaction = store.conn.begin().await.map_err(store_err)?;
        let highest: Option<i64> = entities::agent_run_progress::Entity::find()
            .filter(entities::agent_run_progress::Column::AgentRunId.eq(*run_id.as_uuid()))
            .select_only()
            .column_as(entities::agent_run_progress::Column::Sequence.max(), "max")
            .into_tuple()
            .one(&transaction)
            .await
            .map_err(store_err)?
            .flatten();
        let sequence = highest.unwrap_or(0).saturating_add(1);

        // The unique `(run_id, source_key)` index is what makes a redelivered
        // line a no-op, and the composite primary key is what refuses a
        // sequence two producers assigned at once. Both resolve to "do nothing"
        // here; only the second is worth retrying, and the loop bound keeps
        // even that from spinning.
        let inserted = entities::agent_run_progress::Entity::insert(
            entities::agent_run_progress::ActiveModel {
                agent_run_id: Set(*run_id.as_uuid()),
                sequence: Set(sequence),
                source_key: Set(source_key.clone()),
                text: Set(text.clone()),
                created_at: Set(Utc::now()),
            },
        )
        .on_conflict(OnConflict::new().do_nothing().to_owned())
        .exec_without_returning(&transaction)
        .await;

        match inserted {
            Ok(0) => {
                // Either the key is already published or the sequence was taken.
                // Distinguish them: an existing key is done, a taken sequence
                // wants another assignment.
                let published = entities::agent_run_progress::Entity::find()
                    .filter(entities::agent_run_progress::Column::AgentRunId.eq(*run_id.as_uuid()))
                    .filter(entities::agent_run_progress::Column::SourceKey.eq(source_key.clone()))
                    .count(&transaction)
                    .await
                    .map_err(store_err)?;
                transaction.rollback().await.map_err(store_err)?;
                if published > 0 {
                    return Ok(());
                }
                continue;
            }
            Ok(_) => {}
            Err(_) => {
                transaction.rollback().await.map_err(store_err)?;
                continue;
            }
        }

        // Retention: this stream is disposable observation, and a run that
        // narrates for an hour must not grow the journal without bound.
        let oldest_retained = sequence.saturating_sub(AgentRunProgressEntry::RETAINED_PER_RUN);
        if oldest_retained > 0 {
            entities::agent_run_progress::Entity::delete_many()
                .filter(entities::agent_run_progress::Column::AgentRunId.eq(*run_id.as_uuid()))
                .filter(entities::agent_run_progress::Column::Sequence.lte(oldest_retained))
                .exec(&transaction)
                .await
                .map_err(store_err)?;
        }
        transaction.commit().await.map_err(store_err)?;
        return Ok(());
    }
    Ok(())
}

pub(in crate::db) async fn list(
    store: &DbStore,
    run_id: AgentRunId,
    after_sequence: i64,
    limit: u64,
) -> Result<Vec<AgentRunProgressEntry>> {
    let limit = limit.clamp(1, AgentRunProgressEntry::MAX_PAGE);
    let rows = entities::agent_run_progress::Entity::find()
        .filter(entities::agent_run_progress::Column::AgentRunId.eq(*run_id.as_uuid()))
        .filter(entities::agent_run_progress::Column::Sequence.gt(after_sequence.max(0)))
        .order_by_asc(entities::agent_run_progress::Column::Sequence)
        .limit(limit)
        .all(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(rows
        .into_iter()
        .map(|row| AgentRunProgressEntry {
            run_id: AgentRunId(row.agent_run_id),
            sequence: row.sequence,
            text: row.text,
            created_at: row.created_at,
        })
        .collect())
}

/// Truncate to at most `limit` characters, never splitting one.
fn truncate_on_char_boundary(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    text.chars().take(limit).collect()
}
