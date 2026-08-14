//! Agent-run scheduling and multi-agent wait tables of the schema baseline.

use sea_orm_migration::prelude::*;

use crate::db::migration::idens::*;
use crate::model::{
    AgentRunCancellationReason, AgentRunStatus, AgentRunWaitCondition, TurnAgentRunWaitStatus,
};

/// A claim token is either unbound or fully bound to one run attempt: the
/// disjunction is what keeps a half-populated lease from existing.
pub(super) fn agent_run_claim_table() -> TableCreateStatement {
    Table::create()
        .table(AgentRunClaim::Table)
        .col(
            ColumnDef::new(AgentRunClaim::Token)
                .uuid()
                .not_null()
                .primary_key(),
        )
        .col(ColumnDef::new(AgentRunClaim::AgentRunId).uuid())
        .col(ColumnDef::new(AgentRunClaim::AttemptCount).integer())
        .col(ColumnDef::new(AgentRunClaim::ClaimCount).integer())
        .col(
            ColumnDef::new(AgentRunClaim::ClaimedAt)
                .timestamp_with_time_zone()
                .not_null(),
        )
        .col(ColumnDef::new(AgentRunClaim::LeaseExpiresAt).timestamp_with_time_zone())
        .check(
            Expr::col(AgentRunClaim::AgentRunId)
                .is_null()
                .and(Expr::col(AgentRunClaim::AttemptCount).is_null())
                .and(Expr::col(AgentRunClaim::ClaimCount).is_null())
                .and(Expr::col(AgentRunClaim::LeaseExpiresAt).is_null())
                .or(Expr::col(AgentRunClaim::AgentRunId)
                    .is_not_null()
                    .and(Expr::col(AgentRunClaim::AttemptCount).is_not_null())
                    .and(Expr::col(AgentRunClaim::AttemptCount).gte(1))
                    .and(
                        Expr::col(AgentRunClaim::ClaimCount).is_not_null().and(
                            Expr::col(AgentRunClaim::ClaimCount)
                                .gte(Expr::col(AgentRunClaim::AttemptCount)),
                        ),
                    )
                    .and(
                        Expr::col(AgentRunClaim::LeaseExpiresAt).is_not_null().and(
                            Expr::col(AgentRunClaim::LeaseExpiresAt)
                                .gt(Expr::col(AgentRunClaim::ClaimedAt)),
                        ),
                    )),
        )
        .to_owned()
}

pub(super) fn agent_run_claim_indexes() -> Vec<IndexCreateStatement> {
    vec![
        Index::create()
            .name("idx_agent_run_claim_identity")
            .table(AgentRunClaim::Table)
            .col(AgentRunClaim::Token)
            .col(AgentRunClaim::AgentRunId)
            .col(AgentRunClaim::AttemptCount)
            .col(AgentRunClaim::ClaimCount)
            .unique()
            .to_owned(),
        Index::create()
            .name("idx_agent_run_claim_count")
            .table(AgentRunClaim::Table)
            .col(AgentRunClaim::AgentRunId)
            .col(AgentRunClaim::ClaimCount)
            .unique()
            .to_owned(),
    ]
}

/// An admitted sandbox child carries the turn that spawned it, and a delegated
/// root is a pair or nothing at all.
///
/// `admitted_at` is the discriminator: it is set exactly on the runs that came
/// in through a spawn call, and those are the runs that have an origin turn.
fn agent_run_admission_check() -> SimpleExpr {
    Expr::col(AgentRun::AdmittedAt)
        .is_null()
        .or(Expr::col(AgentRun::OriginTurnId).is_not_null())
        .and(
            Expr::col(AgentRun::DelegatedRootId)
                .is_null()
                .and(Expr::col(AgentRun::DelegatedRelativePath).is_null())
                .or(Expr::col(AgentRun::DelegatedRootId)
                    .is_not_null()
                    .and(Expr::col(AgentRun::DelegatedRelativePath).is_not_null())),
        )
}

pub(super) fn agent_run_table() -> TableCreateStatement {
    Table::create()
        .table(AgentRun::Table)
        .col(ColumnDef::new(AgentRun::Id).uuid().not_null())
        .col(ColumnDef::new(AgentRun::ChatId).uuid().not_null())
        .col(ColumnDef::new(AgentRun::ParentId).uuid())
        .col(ColumnDef::new(AgentRun::ParentDepth).small_integer())
        .col(ColumnDef::new(AgentRun::SpawnCallId).uuid())
        .col(ColumnDef::new(AgentRun::Tier).string_len(16).not_null())
        .col(
            ColumnDef::new(AgentRun::ExecutionLocation)
                .string_len(16)
                .not_null(),
        )
        .col(ColumnDef::new(AgentRun::Depth).small_integer().not_null())
        .col(ColumnDef::new(AgentRun::Status).string_len(32).not_null())
        .col(ColumnDef::new(AgentRun::Input).text())
        .col(
            ColumnDef::new(AgentRun::Model)
                .string_len(crate::model::AgentRun::MAX_MODEL_LEN as u32),
        )
        .col(ColumnDef::new(AgentRun::AttemptCount).integer().not_null())
        .col(ColumnDef::new(AgentRun::MaxAttempts).integer().not_null())
        .col(ColumnDef::new(AgentRun::ClaimCount).integer().not_null())
        .col(
            ColumnDef::new(AgentRun::CheckinGrants)
                .integer()
                .not_null()
                .default(0),
        )
        .col(
            ColumnDef::new(AgentRun::CheckinWatermark)
                .integer()
                .not_null()
                .default(0),
        )
        .col(
            ColumnDef::new(AgentRun::ModelSteps)
                .integer()
                .not_null()
                .default(0),
        )
        .col(
            ColumnDef::new(AgentRun::InputTokens)
                .big_integer()
                .not_null()
                .default(0),
        )
        .col(
            ColumnDef::new(AgentRun::OutputTokens)
                .big_integer()
                .not_null()
                .default(0),
        )
        .col(
            ColumnDef::new(AgentRun::CacheReadInputTokens)
                .big_integer()
                .not_null()
                .default(0),
        )
        .col(
            ColumnDef::new(AgentRun::CacheCreationInputTokens)
                .big_integer()
                .not_null()
                .default(0),
        )
        .col(
            ColumnDef::new(AgentRun::AvailableAt)
                .timestamp_with_time_zone()
                .not_null(),
        )
        .col(ColumnDef::new(AgentRun::DeadlineAt).timestamp_with_time_zone())
        .col(ColumnDef::new(AgentRun::LeaseToken).uuid())
        .col(ColumnDef::new(AgentRun::LeaseExpiresAt).timestamp_with_time_zone())
        .col(ColumnDef::new(AgentRun::StartedAt).timestamp_with_time_zone())
        .col(ColumnDef::new(AgentRun::FinishedAt).timestamp_with_time_zone())
        .col(
            ColumnDef::new(AgentRun::LastErrorCode)
                .string_len(crate::model::AgentRun::MAX_ERROR_CODE_LEN as u32),
        )
        .col(
            ColumnDef::new(AgentRun::LastErrorDetail)
                .string_len(crate::model::AgentRun::MAX_ERROR_DETAIL_LEN as u32),
        )
        // The admission facts for a sandbox-hosted child: the turn that spawned
        // it, the optional delegated root it may reach, and when it was
        // admitted. Null on every run that was not admitted through a spawn
        // call, which is what `admitted_at` discriminates on.
        .col(ColumnDef::new(AgentRun::OriginTurnId).uuid())
        .col(ColumnDef::new(AgentRun::DelegatedRootId).uuid())
        .col(ColumnDef::new(AgentRun::DelegatedRelativePath).text())
        .col(ColumnDef::new(AgentRun::AdmittedAt).timestamp_with_time_zone())
        .col(
            ColumnDef::new(AgentRun::CreatedAt)
                .timestamp_with_time_zone()
                .not_null(),
        )
        .col(
            ColumnDef::new(AgentRun::UpdatedAt)
                .timestamp_with_time_zone()
                .not_null(),
        )
        .primary_key(
            Index::create()
                .name("pk_agent_run")
                .col(AgentRun::Id)
                .col(AgentRun::ChatId)
                .col(AgentRun::Depth),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_agent_run_chat")
                .from(AgentRun::Table, AgentRun::ChatId)
                .to(Chat::Table, Chat::Id)
                .on_delete(ForeignKeyAction::Restrict),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_agent_run_parent")
                .from_tbl(AgentRun::Table)
                .from_col(AgentRun::ParentId)
                .from_col(AgentRun::ChatId)
                .from_col(AgentRun::ParentDepth)
                .to_tbl(AgentRun::Table)
                .to_col(AgentRun::Id)
                .to_col(AgentRun::ChatId)
                .to_col(AgentRun::Depth)
                .on_delete(ForeignKeyAction::Restrict),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_agent_run_live_claim")
                .from_tbl(AgentRun::Table)
                .from_col(AgentRun::LeaseToken)
                .from_col(AgentRun::Id)
                .from_col(AgentRun::AttemptCount)
                .from_col(AgentRun::ClaimCount)
                .to_tbl(AgentRunClaim::Table)
                .to_col(AgentRunClaim::Token)
                .to_col(AgentRunClaim::AgentRunId)
                .to_col(AgentRunClaim::AttemptCount)
                .to_col(AgentRunClaim::ClaimCount)
                .on_delete(ForeignKeyAction::Restrict),
        )
        // `origin_turn_id` carries no foreign key, unlike the admission row it
        // replaces. `turn_run` already references `agent_run`, so a key back the
        // other way is a cycle, and neither engine can add one after both tables
        // exist — SQLite has no `ALTER TABLE ADD CONSTRAINT` at all. The turn is
        // read and fenced in the same transaction that admits the child, which
        // is the only writer that sets this column.
        .check(agent_run_shape_check())
        .check(agent_run_admission_check())
        .check(agent_run_lease_check())
        .check(agent_run_finished_check())
        .check(agent_run_error_check())
        .check(Expr::col(AgentRun::ModelSteps).between(0, i32::MAX))
        .check(Expr::col(AgentRun::InputTokens).between(0, i64::from(u32::MAX)))
        .check(Expr::col(AgentRun::OutputTokens).between(0, i64::from(u32::MAX)))
        .check(Expr::col(AgentRun::CacheReadInputTokens).between(0, i64::from(u32::MAX)))
        .check(Expr::col(AgentRun::CacheCreationInputTokens).between(0, i64::from(u32::MAX)))
        .check(
            Expr::col(AgentRun::LastErrorDetail)
                .is_null()
                .or(Expr::col(AgentRun::LastErrorCode).is_not_null()),
        )
        .check(Expr::col(AgentRun::UpdatedAt).gte(Expr::col(AgentRun::CreatedAt)))
        .check(agent_run_location_check())
        .to_owned()
}

pub(super) fn agent_run_indexes() -> Vec<IndexCreateStatement> {
    vec![
        Index::create()
            .name("idx_agent_run_id")
            .table(AgentRun::Table)
            .col(AgentRun::Id)
            .unique()
            .to_owned(),
        Index::create()
            .name("idx_agent_run_id_parent_chat")
            .table(AgentRun::Table)
            .col(AgentRun::Id)
            .col(AgentRun::ParentId)
            .col(AgentRun::ChatId)
            .unique()
            .to_owned(),
        Index::create()
            .name("idx_agent_run_admission_identity")
            .table(AgentRun::Table)
            .col(AgentRun::Id)
            .col(AgentRun::ParentId)
            .col(AgentRun::ChatId)
            .col(AgentRun::SpawnCallId)
            .unique()
            .to_owned(),
        // Backs `fk_turn_agent_run_wait_member_admission`: a wait member names
        // the child it waits on together with the turn and parent that admitted
        // it, and this is what makes that quadruple addressable.
        Index::create()
            .name("idx_agent_run_wait_owner")
            .table(AgentRun::Table)
            .col(AgentRun::Id)
            .col(AgentRun::OriginTurnId)
            .col(AgentRun::ParentId)
            .col(AgentRun::ChatId)
            .unique()
            .to_owned(),
        // Outstanding admitted children of one turn, oldest first.
        Index::create()
            .name("idx_agent_run_admitted_outstanding")
            .table(AgentRun::Table)
            .col(AgentRun::OriginTurnId)
            .col(AgentRun::AdmittedAt)
            .col(AgentRun::Id)
            .to_owned(),
        Index::create()
            .name("idx_agent_run_spawn_call")
            .table(AgentRun::Table)
            .col(AgentRun::SpawnCallId)
            .unique()
            .to_owned(),
        Index::create()
            .name("idx_agent_run_one_foreground")
            .table(AgentRun::Table)
            .col(AgentRun::ChatId)
            .unique()
            .and_where(Expr::col(AgentRun::Tier).eq("foreground"))
            .to_owned(),
        Index::create()
            .name("idx_agent_run_parent")
            .table(AgentRun::Table)
            .col(AgentRun::ParentId)
            .col(AgentRun::CreatedAt)
            .to_owned(),
        Index::create()
            .name("idx_agent_run_chat_history")
            .table(AgentRun::Table)
            .col(AgentRun::ChatId)
            .col(AgentRun::CreatedAt)
            .col(AgentRun::Id)
            .to_owned(),
        Index::create()
            .name("idx_agent_run_claimable")
            .table(AgentRun::Table)
            .col(AgentRun::Status)
            .col(AgentRun::AvailableAt)
            .col(AgentRun::CreatedAt)
            .col(AgentRun::Id)
            .to_owned(),
        Index::create()
            .name("idx_agent_run_live_by_chat")
            .table(AgentRun::Table)
            .col(AgentRun::Status)
            .col(AgentRun::ChatId)
            .col(AgentRun::LeaseExpiresAt)
            .to_owned(),
    ]
}

/// The foreground/background row-shape constraint. A foreground row is the
/// chat's own coordinator — no parent, no retries, no deadline; a background
/// row is a spawned child with bounded input, attempts, and a deadline.
fn agent_run_shape_check() -> SimpleExpr {
    let foreground_status = Expr::col(AgentRun::Status).is_in([
        AgentRunStatus::Active.as_str(),
        AgentRunStatus::Completed.as_str(),
        AgentRunStatus::Failed.as_str(),
        AgentRunStatus::Cancelled.as_str(),
    ]);
    let background_status = Expr::col(AgentRun::Status).is_in([
        AgentRunStatus::Queued.as_str(),
        AgentRunStatus::Running.as_str(),
        AgentRunStatus::Cancelling.as_str(),
        AgentRunStatus::Waiting.as_str(),
        AgentRunStatus::RetryWait.as_str(),
        AgentRunStatus::NeedsInput.as_str(),
        AgentRunStatus::Completed.as_str(),
        AgentRunStatus::Failed.as_str(),
        AgentRunStatus::Cancelled.as_str(),
    ]);
    let foreground_shape = Expr::col(AgentRun::Tier)
        .eq("foreground")
        .and(Expr::col(AgentRun::Depth).eq(0))
        .and(Expr::col(AgentRun::ParentId).is_null())
        .and(Expr::col(AgentRun::ParentDepth).is_null())
        .and(Expr::col(AgentRun::SpawnCallId).is_null())
        .and(Expr::col(AgentRun::Input).is_null())
        .and(Expr::col(AgentRun::AttemptCount).eq(0))
        .and(Expr::col(AgentRun::MaxAttempts).eq(0))
        .and(Expr::col(AgentRun::ClaimCount).eq(0))
        .and(Expr::col(AgentRun::AvailableAt).eq(Expr::col(AgentRun::CreatedAt)))
        .and(Expr::col(AgentRun::DeadlineAt).is_null())
        .and(Expr::col(AgentRun::StartedAt).is_null())
        .and(foreground_status);
    let background_shape = Expr::col(AgentRun::Tier)
        .eq("background")
        .and(Expr::col(AgentRun::Depth).eq(i32::from(crate::model::AgentRun::MAX_DEPTH)))
        .and(Expr::col(AgentRun::ParentId).is_not_null())
        .and(Expr::col(AgentRun::ParentDepth).eq(0))
        .and(Expr::col(AgentRun::SpawnCallId).is_not_null())
        .and(Expr::col(AgentRun::Input).is_not_null())
        .and(Expr::col(AgentRun::MaxAttempts).gte(1))
        .and(Expr::col(AgentRun::AttemptCount).gte(0))
        .and(Expr::col(AgentRun::AttemptCount).lte(Expr::col(AgentRun::MaxAttempts)))
        .and(Expr::col(AgentRun::ClaimCount).gte(Expr::col(AgentRun::AttemptCount)))
        .and(Expr::col(AgentRun::ClaimCount).lt(i32::MAX))
        .and(Expr::col(AgentRun::AvailableAt).gte(Expr::col(AgentRun::CreatedAt)))
        .and(Expr::col(AgentRun::DeadlineAt).gt(Expr::col(AgentRun::CreatedAt)))
        .and(
            Func::char_length(Expr::col(AgentRun::Input))
                .between(1, crate::model::AgentRun::MAX_INPUT_LEN as i32),
        )
        .and(background_status);
    foreground_shape.or(background_shape)
}

/// The execution-location domain: in-process, or resident in a container the
/// sandbox-resident driver provisions and drives.
fn agent_run_location_check() -> SimpleExpr {
    Expr::col(AgentRun::ExecutionLocation).is_in(["in_process", "container"])
}

/// A lease exists exactly while the run is being executed by a worker.
fn agent_run_lease_check() -> SimpleExpr {
    let active_lease = Expr::col(AgentRun::Status)
        .is_in([
            AgentRunStatus::Running.as_str(),
            AgentRunStatus::Cancelling.as_str(),
        ])
        .and(Expr::col(AgentRun::LeaseToken).is_not_null())
        .and(Expr::col(AgentRun::LeaseExpiresAt).is_not_null())
        .and(Expr::col(AgentRun::AttemptCount).gte(1))
        .and(Expr::col(AgentRun::StartedAt).is_not_null());
    let no_lease = Expr::col(AgentRun::Status)
        .is_not_in([
            AgentRunStatus::Running.as_str(),
            AgentRunStatus::Cancelling.as_str(),
        ])
        .and(Expr::col(AgentRun::LeaseToken).is_null())
        .and(Expr::col(AgentRun::LeaseExpiresAt).is_null());
    active_lease.or(no_lease)
}

fn agent_run_finished_check() -> SimpleExpr {
    let terminal_finished = Expr::col(AgentRun::Status)
        .is_in([
            AgentRunStatus::Completed.as_str(),
            AgentRunStatus::Failed.as_str(),
            AgentRunStatus::Cancelled.as_str(),
        ])
        .and(Expr::col(AgentRun::FinishedAt).is_not_null());
    let nonterminal_unfinished = Expr::col(AgentRun::Status)
        .is_in([
            AgentRunStatus::Active.as_str(),
            AgentRunStatus::Queued.as_str(),
            AgentRunStatus::Running.as_str(),
            AgentRunStatus::Cancelling.as_str(),
            AgentRunStatus::Waiting.as_str(),
            AgentRunStatus::RetryWait.as_str(),
            AgentRunStatus::NeedsInput.as_str(),
        ])
        .and(Expr::col(AgentRun::FinishedAt).is_null());
    terminal_finished.or(nonterminal_unfinished)
}

fn agent_run_error_check() -> SimpleExpr {
    let failure_has_error = Expr::col(AgentRun::Status)
        .is_in([
            AgentRunStatus::RetryWait.as_str(),
            AgentRunStatus::Failed.as_str(),
        ])
        .and(Expr::col(AgentRun::LastErrorCode).is_not_null());
    let success_has_no_error = Expr::col(AgentRun::Status)
        .is_not_in([
            AgentRunStatus::RetryWait.as_str(),
            AgentRunStatus::Failed.as_str(),
        ])
        .and(Expr::col(AgentRun::LastErrorCode).is_null())
        .and(Expr::col(AgentRun::LastErrorDetail).is_null());
    failure_has_error.or(success_has_no_error)
}

/// One result row per run, admitted only under the lease that produced it.
pub(super) fn agent_run_result_table() -> TableCreateStatement {
    Table::create()
        .table(AgentRunResult::Table)
        .col(
            ColumnDef::new(AgentRunResult::AgentRunId)
                .uuid()
                .not_null()
                .primary_key(),
        )
        .col(ColumnDef::new(AgentRunResult::LeaseToken).uuid().not_null())
        .col(
            ColumnDef::new(AgentRunResult::AttemptCount)
                .integer()
                .not_null(),
        )
        .col(
            ColumnDef::new(AgentRunResult::ClaimCount)
                .integer()
                .not_null(),
        )
        .col(ColumnDef::new(AgentRunResult::Text).text().not_null())
        .col(
            ColumnDef::new(AgentRunResult::ModelSteps)
                .integer()
                .not_null()
                .default(0),
        )
        .col(
            ColumnDef::new(AgentRunResult::InputTokens)
                .big_integer()
                .not_null()
                .default(0),
        )
        .col(
            ColumnDef::new(AgentRunResult::OutputTokens)
                .big_integer()
                .not_null()
                .default(0),
        )
        .col(
            ColumnDef::new(AgentRunResult::CacheReadInputTokens)
                .big_integer()
                .not_null()
                .default(0),
        )
        .col(
            ColumnDef::new(AgentRunResult::CacheCreationInputTokens)
                .big_integer()
                .not_null()
                .default(0),
        )
        .col(
            ColumnDef::new(AgentRunResult::SubmittedAt)
                .timestamp_with_time_zone()
                .not_null(),
        )
        .col(
            ColumnDef::new(AgentRunResult::PayloadKind)
                .string_len(32)
                .not_null()
                .default("final_text"),
        )
        .col(
            ColumnDef::new(AgentRunResult::PayloadJson)
                .text()
                .not_null()
                .default("{\\\"text\\\":\\\"\\\"}"),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_agent_run_result_run")
                .from(AgentRunResult::Table, AgentRunResult::AgentRunId)
                .to(AgentRun::Table, AgentRun::Id)
                .on_delete(ForeignKeyAction::Restrict),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_agent_run_result_claim")
                .from_tbl(AgentRunResult::Table)
                .from_col(AgentRunResult::LeaseToken)
                .from_col(AgentRunResult::AgentRunId)
                .from_col(AgentRunResult::AttemptCount)
                .from_col(AgentRunResult::ClaimCount)
                .to_tbl(AgentRunClaim::Table)
                .to_col(AgentRunClaim::Token)
                .to_col(AgentRunClaim::AgentRunId)
                .to_col(AgentRunClaim::AttemptCount)
                .to_col(AgentRunClaim::ClaimCount)
                .on_delete(ForeignKeyAction::Restrict),
        )
        .check(Expr::col(AgentRunResult::AttemptCount).gte(1))
        .check(Expr::col(AgentRunResult::ClaimCount).gte(Expr::col(AgentRunResult::AttemptCount)))
        .check(Expr::col(AgentRunResult::ModelSteps).between(0, i32::MAX))
        .check(Expr::col(AgentRunResult::InputTokens).between(0, i64::from(u32::MAX)))
        .check(Expr::col(AgentRunResult::OutputTokens).between(0, i64::from(u32::MAX)))
        .check(Expr::col(AgentRunResult::CacheReadInputTokens).between(0, i64::from(u32::MAX)))
        .check(Expr::col(AgentRunResult::CacheCreationInputTokens).between(0, i64::from(u32::MAX)))
        .check(
            Func::char_length(Expr::col(AgentRunResult::Text))
                .between(1, crate::model::AgentRun::MAX_RESULT_LEN as i32),
        )
        .to_owned()
}

pub(super) fn agent_run_result_indexes() -> Vec<IndexCreateStatement> {
    vec![Index::create()
        .name("idx_agent_run_result_identity")
        .table(AgentRunResult::Table)
        .col(AgentRunResult::AgentRunId)
        .col(AgentRunResult::LeaseToken)
        .col(AgentRunResult::AttemptCount)
        .col(AgentRunResult::ClaimCount)
        .unique()
        .to_owned()]
}

/// The bounded, ordered progress stream one background run publishes while it
/// works.
///
/// This is observation, not correctness state: nothing reads it to decide what
/// a run may do next, and losing a line only leaves a gap in what an observer
/// sees. Its ordering contract is the per-run `sequence`, which is what lets a
/// reader resume from a cursor instead of re-reading the whole stream.
///
/// `source_key` is the producer's own identity for the line — a sandbox
/// protocol event sequence, or the durable checkpoint a model preamble belongs
/// to. It makes an append idempotent, so a reattached container that redelivers
/// events, or a worker that retries an ambiguous commit, cannot duplicate a
/// line the reader already has.
pub(super) fn agent_run_progress_table() -> TableCreateStatement {
    Table::create()
        .table(AgentRunProgress::Table)
        .col(
            ColumnDef::new(AgentRunProgress::AgentRunId)
                .uuid()
                .not_null(),
        )
        .col(
            ColumnDef::new(AgentRunProgress::Sequence)
                .big_integer()
                .not_null(),
        )
        .col(
            ColumnDef::new(AgentRunProgress::SourceKey)
                .string_len(96)
                .not_null(),
        )
        .col(ColumnDef::new(AgentRunProgress::Text).text().not_null())
        .col(
            ColumnDef::new(AgentRunProgress::CreatedAt)
                .timestamp_with_time_zone()
                .not_null(),
        )
        .primary_key(
            Index::create()
                .col(AgentRunProgress::AgentRunId)
                .col(AgentRunProgress::Sequence),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_agent_run_progress_run")
                .from(AgentRunProgress::Table, AgentRunProgress::AgentRunId)
                .to(AgentRun::Table, AgentRun::Id)
                .on_delete(ForeignKeyAction::Cascade),
        )
        .check(Expr::col(AgentRunProgress::Sequence).gte(1))
        .check(
            Func::char_length(Expr::col(AgentRunProgress::Text))
                .between(1, crate::model::AgentRunProgressEntry::MAX_TEXT_LEN as i32),
        )
        .to_owned()
}

pub(super) fn agent_run_progress_indexes() -> Vec<IndexCreateStatement> {
    vec![Index::create()
        .name("idx_agent_run_progress_source")
        .table(AgentRunProgress::Table)
        .col(AgentRunProgress::AgentRunId)
        .col(AgentRunProgress::SourceKey)
        .unique()
        .to_owned()]
}

/// A cancellation request against a live run, recorded under the lease it
/// interrupts so a stale worker cannot observe someone else's request.
pub(super) fn agent_run_cancellation_table() -> TableCreateStatement {
    Table::create()
        .table(AgentRunCancellation::Table)
        .col(
            ColumnDef::new(AgentRunCancellation::AgentRunId)
                .uuid()
                .not_null()
                .primary_key(),
        )
        .col(
            ColumnDef::new(AgentRunCancellation::LeaseToken)
                .uuid()
                .not_null(),
        )
        .col(
            ColumnDef::new(AgentRunCancellation::AttemptCount)
                .integer()
                .not_null(),
        )
        .col(
            ColumnDef::new(AgentRunCancellation::ClaimCount)
                .integer()
                .not_null(),
        )
        .col(
            ColumnDef::new(AgentRunCancellation::Reason)
                .string()
                .not_null(),
        )
        .col(
            ColumnDef::new(AgentRunCancellation::RequestedAt)
                .timestamp_with_time_zone()
                .not_null(),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_agent_run_cancellation_run")
                .from(
                    AgentRunCancellation::Table,
                    AgentRunCancellation::AgentRunId,
                )
                .to(AgentRun::Table, AgentRun::Id)
                .on_delete(ForeignKeyAction::Restrict),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_agent_run_cancellation_claim")
                .from_tbl(AgentRunCancellation::Table)
                .from_col(AgentRunCancellation::LeaseToken)
                .from_col(AgentRunCancellation::AgentRunId)
                .from_col(AgentRunCancellation::AttemptCount)
                .from_col(AgentRunCancellation::ClaimCount)
                .to_tbl(AgentRunClaim::Table)
                .to_col(AgentRunClaim::Token)
                .to_col(AgentRunClaim::AgentRunId)
                .to_col(AgentRunClaim::AttemptCount)
                .to_col(AgentRunClaim::ClaimCount)
                .on_delete(ForeignKeyAction::Restrict),
        )
        .check(Expr::col(AgentRunCancellation::AttemptCount).gte(1))
        .check(
            Expr::col(AgentRunCancellation::ClaimCount)
                .gte(Expr::col(AgentRunCancellation::AttemptCount)),
        )
        .check(Expr::col(AgentRunCancellation::Reason).is_in([
            AgentRunCancellationReason::Requested.as_str(),
            AgentRunCancellationReason::ParentTurnCancelled.as_str(),
            AgentRunCancellationReason::ParentTurnFailed.as_str(),
        ]))
        .to_owned()
}

pub(super) fn agent_run_cancellation_indexes() -> Vec<IndexCreateStatement> {
    vec![]
}

/// One delivery slot per finished child run, claimed and consumed by the
/// parent exactly once. The status disjunction pins which lease columns may be
/// populated in each state.
pub(super) fn agent_run_inbox_table() -> TableCreateStatement {
    Table::create()
        .table(AgentRunInbox::Table)
        .col(
            ColumnDef::new(AgentRunInbox::ChildRunId)
                .uuid()
                .not_null()
                .primary_key(),
        )
        .col(ColumnDef::new(AgentRunInbox::ParentRunId).uuid().not_null())
        .col(ColumnDef::new(AgentRunInbox::ChatId).uuid().not_null())
        .col(
            ColumnDef::new(AgentRunInbox::ParentDepth)
                .small_integer()
                .not_null(),
        )
        .col(
            ColumnDef::new(AgentRunInbox::ResultLeaseToken)
                .uuid()
                .not_null(),
        )
        .col(
            ColumnDef::new(AgentRunInbox::ResultAttemptCount)
                .integer()
                .not_null(),
        )
        .col(
            ColumnDef::new(AgentRunInbox::ResultClaimCount)
                .integer()
                .not_null(),
        )
        .col(ColumnDef::new(AgentRunInbox::Status).string().not_null())
        .col(
            ColumnDef::new(AgentRunInbox::ClaimCount)
                .integer()
                .not_null(),
        )
        .col(ColumnDef::new(AgentRunInbox::LeaseToken).uuid().null())
        .col(
            ColumnDef::new(AgentRunInbox::LeaseExpiresAt)
                .timestamp_with_time_zone()
                .null(),
        )
        .col(
            ColumnDef::new(AgentRunInbox::ConsumedLeaseToken)
                .uuid()
                .null(),
        )
        .col(
            ColumnDef::new(AgentRunInbox::ConsumedAt)
                .timestamp_with_time_zone()
                .null(),
        )
        .col(
            ColumnDef::new(AgentRunInbox::DeliveredAt)
                .timestamp_with_time_zone()
                .not_null(),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_agent_run_inbox_child")
                .from_tbl(AgentRunInbox::Table)
                .from_col(AgentRunInbox::ChildRunId)
                .from_col(AgentRunInbox::ParentRunId)
                .from_col(AgentRunInbox::ChatId)
                .to_tbl(AgentRun::Table)
                .to_col(AgentRun::Id)
                .to_col(AgentRun::ParentId)
                .to_col(AgentRun::ChatId)
                .on_delete(ForeignKeyAction::Restrict),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_agent_run_inbox_parent")
                .from_tbl(AgentRunInbox::Table)
                .from_col(AgentRunInbox::ParentRunId)
                .from_col(AgentRunInbox::ChatId)
                .from_col(AgentRunInbox::ParentDepth)
                .to_tbl(AgentRun::Table)
                .to_col(AgentRun::Id)
                .to_col(AgentRun::ChatId)
                .to_col(AgentRun::Depth)
                .on_delete(ForeignKeyAction::Restrict),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_agent_run_inbox_result")
                .from_tbl(AgentRunInbox::Table)
                .from_col(AgentRunInbox::ChildRunId)
                .from_col(AgentRunInbox::ResultLeaseToken)
                .from_col(AgentRunInbox::ResultAttemptCount)
                .from_col(AgentRunInbox::ResultClaimCount)
                .to_tbl(AgentRunResult::Table)
                .to_col(AgentRunResult::AgentRunId)
                .to_col(AgentRunResult::LeaseToken)
                .to_col(AgentRunResult::AttemptCount)
                .to_col(AgentRunResult::ClaimCount)
                .on_delete(ForeignKeyAction::Restrict),
        )
        .check(Expr::col(AgentRunInbox::ParentDepth).eq(0))
        .check(Expr::col(AgentRunInbox::ResultAttemptCount).gte(1))
        .check(
            Expr::col(AgentRunInbox::ResultClaimCount)
                .gte(Expr::col(AgentRunInbox::ResultAttemptCount)),
        )
        .check(Expr::col(AgentRunInbox::Status).is_in([
            "pending",
            "claimed",
            "consumed",
            "cancelled",
        ]))
        .check(Expr::col(AgentRunInbox::ClaimCount).gte(0))
        .check(
            Expr::col(AgentRunInbox::Status)
                .eq("pending")
                .and(Expr::col(AgentRunInbox::ClaimCount).eq(0))
                .and(Expr::col(AgentRunInbox::LeaseToken).is_null())
                .and(Expr::col(AgentRunInbox::LeaseExpiresAt).is_null())
                .and(Expr::col(AgentRunInbox::ConsumedLeaseToken).is_null())
                .and(Expr::col(AgentRunInbox::ConsumedAt).is_null())
                .or(Expr::col(AgentRunInbox::Status)
                    .eq("claimed")
                    .and(Expr::col(AgentRunInbox::ClaimCount).gte(1))
                    .and(Expr::col(AgentRunInbox::LeaseToken).is_not_null())
                    .and(Expr::col(AgentRunInbox::LeaseExpiresAt).is_not_null())
                    .and(Expr::col(AgentRunInbox::ConsumedLeaseToken).is_null())
                    .and(Expr::col(AgentRunInbox::ConsumedAt).is_null()))
                .or(Expr::col(AgentRunInbox::Status)
                    .eq("consumed")
                    .and(Expr::col(AgentRunInbox::ClaimCount).gte(1))
                    .and(Expr::col(AgentRunInbox::LeaseToken).is_null())
                    .and(Expr::col(AgentRunInbox::LeaseExpiresAt).is_null())
                    .and(Expr::col(AgentRunInbox::ConsumedLeaseToken).is_not_null())
                    .and(Expr::col(AgentRunInbox::ConsumedAt).is_not_null()))
                .or(Expr::col(AgentRunInbox::Status)
                    .eq("cancelled")
                    .and(Expr::col(AgentRunInbox::LeaseToken).is_null())
                    .and(Expr::col(AgentRunInbox::LeaseExpiresAt).is_null())
                    .and(Expr::col(AgentRunInbox::ConsumedLeaseToken).is_null())
                    .and(Expr::col(AgentRunInbox::ConsumedAt).is_null())),
        )
        .to_owned()
}

pub(super) fn agent_run_inbox_indexes() -> Vec<IndexCreateStatement> {
    vec![]
}

/// A parked turn waiting on a *set* of child runs. The row doubles as the
/// tool call that will be answered on resume, so it carries the call's
/// identity, arguments, and journal position alongside the park accounting.
pub(super) fn turn_agent_run_wait_set_table() -> TableCreateStatement {
    Table::create()
        .table(TurnAgentRunWaitSet::Table)
        .col(
            ColumnDef::new(TurnAgentRunWaitSet::Id)
                .uuid()
                .not_null()
                .primary_key(),
        )
        .col(
            ColumnDef::new(TurnAgentRunWaitSet::ParentRunId)
                .uuid()
                .not_null(),
        )
        .col(
            ColumnDef::new(TurnAgentRunWaitSet::TurnId)
                .uuid()
                .not_null(),
        )
        .col(
            ColumnDef::new(TurnAgentRunWaitSet::ChatId)
                .uuid()
                .not_null(),
        )
        .col(
            ColumnDef::new(TurnAgentRunWaitSet::Condition)
                .text()
                .not_null(),
        )
        .col(
            ColumnDef::new(TurnAgentRunWaitSet::ParkLeaseToken)
                .uuid()
                .not_null(),
        )
        .col(
            ColumnDef::new(TurnAgentRunWaitSet::ExpectedSteerRevision)
                .big_integer()
                .not_null(),
        )
        .col(
            ColumnDef::new(TurnAgentRunWaitSet::AttemptCount)
                .integer()
                .not_null(),
        )
        .col(
            ColumnDef::new(TurnAgentRunWaitSet::ClaimCount)
                .integer()
                .not_null(),
        )
        .col(
            ColumnDef::new(TurnAgentRunWaitSet::ModelSteps)
                .integer()
                .not_null(),
        )
        .col(
            ColumnDef::new(TurnAgentRunWaitSet::InputTokens)
                .big_integer()
                .not_null(),
        )
        .col(
            ColumnDef::new(TurnAgentRunWaitSet::OutputTokens)
                .big_integer()
                .not_null(),
        )
        .col(
            ColumnDef::new(TurnAgentRunWaitSet::CacheReadInputTokens)
                .big_integer()
                .not_null(),
        )
        .col(
            ColumnDef::new(TurnAgentRunWaitSet::CacheCreationInputTokens)
                .big_integer()
                .not_null(),
        )
        .col(
            ColumnDef::new(TurnAgentRunWaitSet::EventOrdinal)
                .integer()
                .not_null(),
        )
        .col(ColumnDef::new(TurnAgentRunWaitSet::EventSeq).big_integer())
        .col(
            ColumnDef::new(TurnAgentRunWaitSet::Status)
                .text()
                .not_null(),
        )
        .col(
            ColumnDef::new(TurnAgentRunWaitSet::ParkedAt)
                .timestamp_with_time_zone()
                .not_null(),
        )
        .col(ColumnDef::new(TurnAgentRunWaitSet::ClosedAt).timestamp_with_time_zone())
        .col(ColumnDef::new(TurnAgentRunWaitSet::ResumeToken).uuid())
        .foreign_key(
            ForeignKey::create()
                .name("fk_turn_agent_run_wait_set_turn")
                .from_tbl(TurnAgentRunWaitSet::Table)
                .from_col(TurnAgentRunWaitSet::TurnId)
                .from_col(TurnAgentRunWaitSet::ChatId)
                .from_col(TurnAgentRunWaitSet::ParentRunId)
                .to_tbl(TurnRun::Table)
                .to_col(TurnRun::Id)
                .to_col(TurnRun::ChatId)
                .to_col(TurnRun::AgentRunId)
                .on_delete(ForeignKeyAction::Restrict),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_turn_agent_run_wait_set_tool")
                .from_tbl(TurnAgentRunWaitSet::Table)
                .from_col(TurnAgentRunWaitSet::Id)
                .from_col(TurnAgentRunWaitSet::ChatId)
                .from_col(TurnAgentRunWaitSet::TurnId)
                .to_tbl(ToolCall::Table)
                .to_col(ToolCall::Id)
                .to_col(ToolCall::ChatId)
                .to_col(ToolCall::TurnId)
                .on_delete(ForeignKeyAction::Restrict),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_turn_agent_run_wait_set_event")
                .from_tbl(TurnAgentRunWaitSet::Table)
                .from_col(TurnAgentRunWaitSet::ChatId)
                .from_col(TurnAgentRunWaitSet::EventSeq)
                .to_tbl(Event::Table)
                .to_col(Event::ChatId)
                .to_col(Event::Seq)
                .on_delete(ForeignKeyAction::Restrict),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_turn_agent_run_wait_set_claim")
                .from_tbl(TurnAgentRunWaitSet::Table)
                .from_col(TurnAgentRunWaitSet::ParkLeaseToken)
                .from_col(TurnAgentRunWaitSet::TurnId)
                .from_col(TurnAgentRunWaitSet::AttemptCount)
                .from_col(TurnAgentRunWaitSet::ClaimCount)
                .to_tbl(TurnClaim::Table)
                .to_col(TurnClaim::Token)
                .to_col(TurnClaim::TurnId)
                .to_col(TurnClaim::AttemptCount)
                .to_col(TurnClaim::ClaimCount)
                .on_delete(ForeignKeyAction::Restrict),
        )
        .check(Expr::col(TurnAgentRunWaitSet::Condition).eq(AgentRunWaitCondition::All.as_str()))
        .check(Expr::col(TurnAgentRunWaitSet::ExpectedSteerRevision).gte(0))
        .check(Expr::col(TurnAgentRunWaitSet::EventOrdinal).between(2, i32::MAX - 1))
        .check(Expr::col(TurnAgentRunWaitSet::AttemptCount).gte(1))
        .check(
            Expr::col(TurnAgentRunWaitSet::ClaimCount)
                .gte(Expr::col(TurnAgentRunWaitSet::AttemptCount)),
        )
        .check(
            Expr::col(TurnAgentRunWaitSet::ModelSteps)
                .gt(0)
                .and(Expr::col(TurnAgentRunWaitSet::InputTokens).gte(0))
                .and(Expr::col(TurnAgentRunWaitSet::OutputTokens).gte(0))
                .and(Expr::col(TurnAgentRunWaitSet::CacheReadInputTokens).gte(0))
                .and(Expr::col(TurnAgentRunWaitSet::CacheCreationInputTokens).gte(0)),
        )
        .check(
            Expr::col(TurnAgentRunWaitSet::Status)
                .eq(TurnAgentRunWaitStatus::Waiting.as_str())
                .and(Expr::col(TurnAgentRunWaitSet::ClosedAt).is_null())
                .and(Expr::col(TurnAgentRunWaitSet::ResumeToken).is_null())
                .and(Expr::col(TurnAgentRunWaitSet::EventSeq).is_null())
                .or(Expr::col(TurnAgentRunWaitSet::Status)
                    .eq(TurnAgentRunWaitStatus::Resumed.as_str())
                    .and(Expr::col(TurnAgentRunWaitSet::ClosedAt).is_not_null())
                    .and(Expr::col(TurnAgentRunWaitSet::ResumeToken).is_not_null())
                    .and(Expr::col(TurnAgentRunWaitSet::EventSeq).is_not_null()))
                .or(Expr::col(TurnAgentRunWaitSet::Status)
                    .eq(TurnAgentRunWaitStatus::Cancelled.as_str())
                    .and(Expr::col(TurnAgentRunWaitSet::ClosedAt).is_not_null())
                    .and(Expr::col(TurnAgentRunWaitSet::ResumeToken).is_null())
                    .and(Expr::col(TurnAgentRunWaitSet::EventSeq).is_not_null())),
        )
        .check(Expr::col(TurnAgentRunWaitSet::ClosedAt).is_null().or(
            Expr::col(TurnAgentRunWaitSet::ClosedAt).gte(Expr::col(TurnAgentRunWaitSet::ParkedAt)),
        ))
        .to_owned()
}

pub(super) fn turn_agent_run_wait_set_indexes() -> Vec<IndexCreateStatement> {
    vec![
        Index::create()
            .name("idx_turn_agent_run_wait_set_member_owner")
            .table(TurnAgentRunWaitSet::Table)
            .col(TurnAgentRunWaitSet::Id)
            .col(TurnAgentRunWaitSet::TurnId)
            .col(TurnAgentRunWaitSet::ParentRunId)
            .col(TurnAgentRunWaitSet::ChatId)
            .unique()
            .to_owned(),
        Index::create()
            .name("idx_turn_agent_run_wait_set_one_open")
            .table(TurnAgentRunWaitSet::Table)
            .col(TurnAgentRunWaitSet::TurnId)
            .unique()
            .and_where(
                Expr::col(TurnAgentRunWaitSet::Status).eq(TurnAgentRunWaitStatus::Waiting.as_str()),
            )
            .to_owned(),
    ]
}

/// One child run a wait set is waiting on. Membership cascades with the set,
/// and the partial unique index keeps a child in at most one open wait.
pub(super) fn turn_agent_run_wait_member_table() -> TableCreateStatement {
    Table::create()
        .table(TurnAgentRunWaitMember::Table)
        .col(
            ColumnDef::new(TurnAgentRunWaitMember::WaitId)
                .uuid()
                .not_null(),
        )
        .col(
            ColumnDef::new(TurnAgentRunWaitMember::Position)
                .small_integer()
                .not_null(),
        )
        .col(
            ColumnDef::new(TurnAgentRunWaitMember::ChildRunId)
                .uuid()
                .not_null(),
        )
        .col(
            ColumnDef::new(TurnAgentRunWaitMember::ParentRunId)
                .uuid()
                .not_null(),
        )
        .col(
            ColumnDef::new(TurnAgentRunWaitMember::OriginTurnId)
                .uuid()
                .not_null(),
        )
        .col(
            ColumnDef::new(TurnAgentRunWaitMember::ChatId)
                .uuid()
                .not_null(),
        )
        .col(
            ColumnDef::new(TurnAgentRunWaitMember::Open)
                .boolean()
                .not_null(),
        )
        .primary_key(
            Index::create()
                .col(TurnAgentRunWaitMember::WaitId)
                .col(TurnAgentRunWaitMember::Position),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_turn_agent_run_wait_member_set")
                .from_tbl(TurnAgentRunWaitMember::Table)
                .from_col(TurnAgentRunWaitMember::WaitId)
                .from_col(TurnAgentRunWaitMember::OriginTurnId)
                .from_col(TurnAgentRunWaitMember::ParentRunId)
                .from_col(TurnAgentRunWaitMember::ChatId)
                .to_tbl(TurnAgentRunWaitSet::Table)
                .to_col(TurnAgentRunWaitSet::Id)
                .to_col(TurnAgentRunWaitSet::TurnId)
                .to_col(TurnAgentRunWaitSet::ParentRunId)
                .to_col(TurnAgentRunWaitSet::ChatId)
                .on_delete(ForeignKeyAction::Cascade),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_turn_agent_run_wait_member_admission")
                .from_tbl(TurnAgentRunWaitMember::Table)
                .from_col(TurnAgentRunWaitMember::ChildRunId)
                .from_col(TurnAgentRunWaitMember::OriginTurnId)
                .from_col(TurnAgentRunWaitMember::ParentRunId)
                .from_col(TurnAgentRunWaitMember::ChatId)
                .to_tbl(AgentRun::Table)
                .to_col(AgentRun::Id)
                .to_col(AgentRun::OriginTurnId)
                .to_col(AgentRun::ParentId)
                .to_col(AgentRun::ChatId)
                .on_delete(ForeignKeyAction::Restrict),
        )
        .check(Expr::col(TurnAgentRunWaitMember::Position).gte(0))
        .check(
            Expr::col(TurnAgentRunWaitMember::Position)
                .lt(crate::model::TurnAgentRunWaitSet::MAX_CHILDREN as i32),
        )
        .to_owned()
}

pub(super) fn turn_agent_run_wait_member_indexes() -> Vec<IndexCreateStatement> {
    vec![Index::create()
        .name("idx_turn_agent_run_wait_member_one_open_child")
        .table(TurnAgentRunWaitMember::Table)
        .col(TurnAgentRunWaitMember::ChildRunId)
        .unique()
        .and_where(Expr::col(TurnAgentRunWaitMember::Open).eq(true))
        .to_owned()]
}
