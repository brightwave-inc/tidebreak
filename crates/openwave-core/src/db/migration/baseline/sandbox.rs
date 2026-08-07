//! Sandbox execution and exec-file tables of the schema baseline.

use sea_orm_migration::prelude::*;

use crate::db::migration::idens::*;
// The iden enum for the journal table is `ExecFileChange`; the model enum of
// the same name is what its `change_kind` column holds.
use crate::model::{
    ExecFileChange as ExecFileChangeKind, ExecFileChangeClassification, ExecFileRejectionReason,
    ExecUndoState, SandboxToolCallStatus, ToolCallRecord,
};

/// The durable sandbox provisioning record: the intent a container run commits
/// — carrying its host-minted correlation tag and provisioning window —
/// *before* the backend is asked to create a sandbox, the handle committed onto
/// it afterwards, and the teardown obligation the sweep drives to completion.
///
/// Recovery is driven by this row, not by what the provider reports: an
/// `intended` record whose window lapses failed its admission whether or not a
/// create ever reached the provider, and the orphan sweep reclaims any provider
/// sandbox whose tag names no live record.
///
/// `late_result_evidence` retains a well-formed result that arrives after its
/// container run is already terminal: such a result fails the fenced commit
/// predicate and must never commit, but it is still evidence of what the
/// container produced.
///
/// `admission` records what the run may admit. Its value domain is enforced at
/// the read boundary — anything unrecognized reads as `attached_only` — rather
/// than by a CHECK.
pub(super) fn sandbox_provision_table() -> TableCreateStatement {
    Table::create()
        .table(SandboxProvision::Table)
        .col(
            ColumnDef::new(SandboxProvision::RunId)
                .uuid()
                .not_null()
                .primary_key(),
        )
        .col(
            ColumnDef::new(SandboxProvision::Tag)
                .string_len(64)
                .not_null()
                .unique_key(),
        )
        .col(
            ColumnDef::new(SandboxProvision::State)
                .string_len(16)
                .not_null(),
        )
        .col(ColumnDef::new(SandboxProvision::Handle).string().null())
        .col(
            ColumnDef::new(SandboxProvision::WindowExpiresAt)
                .timestamp_with_time_zone()
                .not_null(),
        )
        .col(
            ColumnDef::new(SandboxProvision::CreatedAt)
                .timestamp_with_time_zone()
                .not_null(),
        )
        .col(
            ColumnDef::new(SandboxProvision::UpdatedAt)
                .timestamp_with_time_zone()
                .not_null(),
        )
        .col(
            ColumnDef::new(SandboxProvision::LateResultEvidence)
                .text()
                .null(),
        )
        .col(
            ColumnDef::new(SandboxProvision::Admission)
                .string_len(16)
                .not_null()
                .default("attached_only"),
        )
        .check(Expr::col(SandboxProvision::State).is_in([
            "intended",
            "committed",
            "teardown",
            "done",
        ]))
        // An intent has no handle until the backend's create returns.
        .check(
            Expr::col(SandboxProvision::State)
                .ne("intended")
                .or(Expr::col(SandboxProvision::Handle).is_null()),
        )
        // A committed record always has one.
        .check(
            Expr::col(SandboxProvision::State)
                .ne("committed")
                .or(Expr::col(SandboxProvision::Handle).is_not_null()),
        )
        .to_owned()
}

pub(super) fn sandbox_provision_indexes() -> Vec<IndexCreateStatement> {
    vec![]
}

/// The admission record for a sandbox-hosted child agent run: the spawn call
/// that admitted it, the parent run and origin turn it belongs to, and the
/// optional delegated root the child may reach.
pub(super) fn sandbox_agent_admission_table() -> TableCreateStatement {
    Table::create()
        .table(SandboxAgentAdmission::Table)
        .col(
            ColumnDef::new(SandboxAgentAdmission::ChildRunId)
                .uuid()
                .not_null()
                .primary_key(),
        )
        .col(
            ColumnDef::new(SandboxAgentAdmission::ParentRunId)
                .uuid()
                .not_null(),
        )
        .col(
            ColumnDef::new(SandboxAgentAdmission::OriginTurnId)
                .uuid()
                .not_null(),
        )
        .col(
            ColumnDef::new(SandboxAgentAdmission::ChatId)
                .uuid()
                .not_null(),
        )
        .col(
            ColumnDef::new(SandboxAgentAdmission::SpawnCallId)
                .uuid()
                .not_null()
                .unique_key(),
        )
        .col(ColumnDef::new(SandboxAgentAdmission::DelegatedRootId).uuid())
        .col(ColumnDef::new(SandboxAgentAdmission::DelegatedRelativePath).text())
        .col(
            ColumnDef::new(SandboxAgentAdmission::AdmittedAt)
                .timestamp_with_time_zone()
                .not_null(),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_sandbox_agent_admission_child")
                .from_tbl(SandboxAgentAdmission::Table)
                .from_col(SandboxAgentAdmission::ChildRunId)
                .from_col(SandboxAgentAdmission::ParentRunId)
                .from_col(SandboxAgentAdmission::ChatId)
                .from_col(SandboxAgentAdmission::SpawnCallId)
                .to_tbl(AgentRun::Table)
                .to_col(AgentRun::Id)
                .to_col(AgentRun::ParentId)
                .to_col(AgentRun::ChatId)
                .to_col(AgentRun::SpawnCallId)
                .on_delete(ForeignKeyAction::Cascade),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_sandbox_agent_admission_origin_turn")
                .from_tbl(SandboxAgentAdmission::Table)
                .from_col(SandboxAgentAdmission::OriginTurnId)
                .from_col(SandboxAgentAdmission::ChatId)
                .from_col(SandboxAgentAdmission::ParentRunId)
                .to_tbl(TurnRun::Table)
                .to_col(TurnRun::Id)
                .to_col(TurnRun::ChatId)
                .to_col(TurnRun::AgentRunId)
                .on_delete(ForeignKeyAction::Cascade),
        )
        // A delegated root is a pair or nothing at all.
        .check(
            Expr::col(SandboxAgentAdmission::DelegatedRootId)
                .is_null()
                .and(Expr::col(SandboxAgentAdmission::DelegatedRelativePath).is_null())
                .or(Expr::col(SandboxAgentAdmission::DelegatedRootId)
                    .is_not_null()
                    .and(Expr::col(SandboxAgentAdmission::DelegatedRelativePath).is_not_null())),
        )
        .to_owned()
}

pub(super) fn sandbox_agent_admission_indexes() -> Vec<IndexCreateStatement> {
    vec![
        Index::create()
            .name("idx_sandbox_agent_admission_outstanding")
            .table(SandboxAgentAdmission::Table)
            .col(SandboxAgentAdmission::OriginTurnId)
            .col(SandboxAgentAdmission::AdmittedAt)
            .col(SandboxAgentAdmission::ChildRunId)
            .to_owned(),
        Index::create()
            .name("idx_sandbox_agent_admission_wait_owner")
            .table(SandboxAgentAdmission::Table)
            .col(SandboxAgentAdmission::ChildRunId)
            .col(SandboxAgentAdmission::OriginTurnId)
            .col(SandboxAgentAdmission::ParentRunId)
            .col(SandboxAgentAdmission::ChatId)
            .unique()
            .to_owned(),
    ]
}

/// A tool call a sandboxed agent run issued, parked against the claim that
/// owns the run and leased out to whichever executor picks it up.
/// `retry_wait` parks one classified-transient failure until its single
/// bounded retry becomes claimable. The terminal outcome the executor reported
/// lands on the row itself, in the transaction that fences the lease.
pub(super) fn sandbox_tool_call_table() -> TableCreateStatement {
    Table::create()
        .table(SandboxToolCall::Table)
        .col(
            ColumnDef::new(SandboxToolCall::Id)
                .uuid()
                .not_null()
                .primary_key(),
        )
        .col(
            ColumnDef::new(SandboxToolCall::AgentRunId)
                .uuid()
                .not_null(),
        )
        .col(ColumnDef::new(SandboxToolCall::ChatId).uuid().not_null())
        .col(
            ColumnDef::new(SandboxToolCall::AgentRunDepth)
                .small_integer()
                .not_null(),
        )
        .col(
            ColumnDef::new(SandboxToolCall::ProviderId)
                .text()
                .not_null(),
        )
        .col(ColumnDef::new(SandboxToolCall::Name).text().not_null())
        .col(
            ColumnDef::new(SandboxToolCall::Arguments)
                .json_binary()
                .not_null(),
        )
        .col(ColumnDef::new(SandboxToolCall::Status).text().not_null())
        .col(
            ColumnDef::new(SandboxToolCall::ParkLeaseToken)
                .uuid()
                .not_null(),
        )
        .col(
            ColumnDef::new(SandboxToolCall::ParkAttemptCount)
                .integer()
                .not_null(),
        )
        .col(
            ColumnDef::new(SandboxToolCall::ParkClaimCount)
                .integer()
                .not_null(),
        )
        // Position of this call within the model step that parked it. The step
        // emits its calls in one completion, and the transcript replays them in
        // that order, so the order has to survive as data rather than as a
        // property of insertion.
        .col(
            ColumnDef::new(SandboxToolCall::BatchOrdinal)
                .small_integer()
                .not_null(),
        )
        .col(ColumnDef::new(SandboxToolCall::ExecutorLeaseToken).uuid())
        .col(ColumnDef::new(SandboxToolCall::ExecutorLeaseExpiresAt).timestamp_with_time_zone())
        .col(ColumnDef::new(SandboxToolCall::RetryAt).timestamp_with_time_zone())
        // The terminal outcome, written in the same transaction that moves the
        // call to a terminal status. `resolution_lease_token` records which
        // lease produced it — the live executor lease is cleared on resolution,
        // so it cannot answer that afterwards.
        .col(ColumnDef::new(SandboxToolCall::ResolutionLeaseToken).uuid())
        .col(ColumnDef::new(SandboxToolCall::Result).text())
        .col(ColumnDef::new(SandboxToolCall::ErrorCode).text())
        .col(ColumnDef::new(SandboxToolCall::ErrorDetail).text())
        .col(
            ColumnDef::new(SandboxToolCall::CreatedAt)
                .timestamp_with_time_zone()
                .not_null(),
        )
        .col(ColumnDef::new(SandboxToolCall::ResolvedAt).timestamp_with_time_zone())
        .foreign_key(
            ForeignKey::create()
                .name("fk_sandbox_tool_call_run")
                .from_tbl(SandboxToolCall::Table)
                .from_col(SandboxToolCall::AgentRunId)
                .from_col(SandboxToolCall::ChatId)
                .from_col(SandboxToolCall::AgentRunDepth)
                .to_tbl(AgentRun::Table)
                .to_col(AgentRun::Id)
                .to_col(AgentRun::ChatId)
                .to_col(AgentRun::Depth)
                .on_delete(ForeignKeyAction::Restrict),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_sandbox_tool_call_park_claim")
                .from_tbl(SandboxToolCall::Table)
                .from_col(SandboxToolCall::ParkLeaseToken)
                .from_col(SandboxToolCall::AgentRunId)
                .from_col(SandboxToolCall::ParkAttemptCount)
                .from_col(SandboxToolCall::ParkClaimCount)
                .to_tbl(AgentRunClaim::Table)
                .to_col(AgentRunClaim::Token)
                .to_col(AgentRunClaim::AgentRunId)
                .to_col(AgentRunClaim::AttemptCount)
                .to_col(AgentRunClaim::ClaimCount)
                .on_delete(ForeignKeyAction::Restrict),
        )
        .check(Expr::col(SandboxToolCall::AgentRunDepth).eq(1))
        .check(Expr::col(SandboxToolCall::ParkAttemptCount).gte(1))
        .check(Expr::col(SandboxToolCall::BatchOrdinal).gte(0))
        .check(
            Expr::col(SandboxToolCall::ParkClaimCount)
                .gte(Expr::col(SandboxToolCall::ParkAttemptCount)),
        )
        .check(Expr::col(SandboxToolCall::Status).is_in([
            SandboxToolCallStatus::Accepted.as_str(),
            SandboxToolCallStatus::Claimed.as_str(),
            SandboxToolCallStatus::RetryWait.as_str(),
            SandboxToolCallStatus::Completed.as_str(),
            SandboxToolCallStatus::Failed.as_str(),
            SandboxToolCallStatus::Cancelled.as_str(),
        ]))
        .check(
            Expr::col(SandboxToolCall::ResolvedAt)
                .is_null()
                .or(Expr::col(SandboxToolCall::ResolvedAt)
                    .gte(Expr::col(SandboxToolCall::CreatedAt))),
        )
        // A resolution lands whole or not at all: a call either carries its
        // outcome, its producing lease, and the moment it settled, or none of
        // the three.
        .check(
            Expr::col(SandboxToolCall::Result)
                .is_null()
                .and(Expr::col(SandboxToolCall::ResolutionLeaseToken).is_null())
                .and(Expr::col(SandboxToolCall::ResolvedAt).is_null())
                .or(Expr::col(SandboxToolCall::Result)
                    .is_not_null()
                    .and(Expr::col(SandboxToolCall::ResolutionLeaseToken).is_not_null())
                    .and(Expr::col(SandboxToolCall::ResolvedAt).is_not_null())),
        )
        // Only a failure carries an error, and a failure always carries a code.
        .check(
            Expr::col(SandboxToolCall::Status)
                .eq(SandboxToolCallStatus::Failed.as_str())
                .and(Expr::col(SandboxToolCall::ErrorCode).is_not_null())
                .or(Expr::col(SandboxToolCall::Status)
                    .is_not_in([SandboxToolCallStatus::Failed.as_str()])
                    .and(Expr::col(SandboxToolCall::ErrorCode).is_null())
                    .and(Expr::col(SandboxToolCall::ErrorDetail).is_null())),
        )
        .to_owned()
}

pub(super) fn sandbox_tool_call_indexes() -> Vec<IndexCreateStatement> {
    vec![
        Index::create()
            .name("idx_sandbox_tool_call_run")
            .table(SandboxToolCall::Table)
            .col(SandboxToolCall::AgentRunId)
            .col(SandboxToolCall::CreatedAt)
            .to_owned(),
        Index::create()
            .name("idx_sandbox_tool_call_recovery")
            .table(SandboxToolCall::Table)
            .col(SandboxToolCall::Status)
            .col(SandboxToolCall::ExecutorLeaseExpiresAt)
            .col(SandboxToolCall::CreatedAt)
            .col(SandboxToolCall::Id)
            .to_owned(),
        // One position per step: a recovered park that re-ran its insert cannot
        // land a second row at an ordinal the first one already occupies.
        Index::create()
            .name("idx_sandbox_tool_call_batch")
            .table(SandboxToolCall::Table)
            .col(SandboxToolCall::AgentRunId)
            .col(SandboxToolCall::ParkAttemptCount)
            .col(SandboxToolCall::ParkClaimCount)
            .col(SandboxToolCall::BatchOrdinal)
            .unique()
            .to_owned(),
    ]
}

/// The single fenced commit of a spawn tool call: the admitted child run, the
/// turn claim that authorized the write, the tool-call row it resolves, and the
/// journal event it appended — all pinned together so the commit either lands
/// whole or not at all.
pub(super) fn sandbox_spawn_checkpoint_table() -> TableCreateStatement {
    Table::create()
        .table(SandboxSpawnCheckpoint::Table)
        .col(
            ColumnDef::new(SandboxSpawnCheckpoint::CallId)
                .uuid()
                .not_null()
                .primary_key(),
        )
        .col(
            ColumnDef::new(SandboxSpawnCheckpoint::ChildRunId)
                .uuid()
                .not_null()
                .unique_key(),
        )
        .col(
            ColumnDef::new(SandboxSpawnCheckpoint::ParentRunId)
                .uuid()
                .not_null(),
        )
        .col(
            ColumnDef::new(SandboxSpawnCheckpoint::OriginTurnId)
                .uuid()
                .not_null(),
        )
        .col(
            ColumnDef::new(SandboxSpawnCheckpoint::ChatId)
                .uuid()
                .not_null(),
        )
        .col(
            ColumnDef::new(SandboxSpawnCheckpoint::LeaseToken)
                .uuid()
                .not_null()
                .unique_key(),
        )
        .col(
            ColumnDef::new(SandboxSpawnCheckpoint::AttemptCount)
                .integer()
                .not_null(),
        )
        .col(
            ColumnDef::new(SandboxSpawnCheckpoint::ClaimCount)
                .integer()
                .not_null(),
        )
        .col(
            ColumnDef::new(SandboxSpawnCheckpoint::ProviderId)
                .text()
                .not_null(),
        )
        .col(
            ColumnDef::new(SandboxSpawnCheckpoint::HistoryOrder)
                .big_integer()
                .not_null(),
        )
        .col(
            ColumnDef::new(SandboxSpawnCheckpoint::Arguments)
                .json_binary()
                .not_null(),
        )
        .col(
            ColumnDef::new(SandboxSpawnCheckpoint::Result)
                .text()
                .not_null(),
        )
        .col(
            ColumnDef::new(SandboxSpawnCheckpoint::RemainingRequests)
                .json_binary()
                .not_null(),
        )
        .col(
            ColumnDef::new(SandboxSpawnCheckpoint::SteerRevision)
                .big_integer()
                .not_null(),
        )
        .col(
            ColumnDef::new(SandboxSpawnCheckpoint::EventOrdinal)
                .integer()
                .not_null(),
        )
        .col(
            ColumnDef::new(SandboxSpawnCheckpoint::ModelSteps)
                .integer()
                .not_null(),
        )
        .col(
            ColumnDef::new(SandboxSpawnCheckpoint::InputTokens)
                .big_integer()
                .not_null(),
        )
        .col(
            ColumnDef::new(SandboxSpawnCheckpoint::OutputTokens)
                .big_integer()
                .not_null(),
        )
        .col(
            ColumnDef::new(SandboxSpawnCheckpoint::CacheReadInputTokens)
                .big_integer()
                .not_null(),
        )
        .col(
            ColumnDef::new(SandboxSpawnCheckpoint::CacheCreationInputTokens)
                .big_integer()
                .not_null(),
        )
        .col(
            ColumnDef::new(SandboxSpawnCheckpoint::EventSeq)
                .big_integer()
                .not_null(),
        )
        .col(
            ColumnDef::new(SandboxSpawnCheckpoint::CommittedAt)
                .timestamp_with_time_zone()
                .not_null(),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_sandbox_spawn_checkpoint_admission")
                .from_tbl(SandboxSpawnCheckpoint::Table)
                .from_col(SandboxSpawnCheckpoint::ChildRunId)
                .from_col(SandboxSpawnCheckpoint::OriginTurnId)
                .from_col(SandboxSpawnCheckpoint::ParentRunId)
                .from_col(SandboxSpawnCheckpoint::ChatId)
                .to_tbl(SandboxAgentAdmission::Table)
                .to_col(SandboxAgentAdmission::ChildRunId)
                .to_col(SandboxAgentAdmission::OriginTurnId)
                .to_col(SandboxAgentAdmission::ParentRunId)
                .to_col(SandboxAgentAdmission::ChatId)
                .on_delete(ForeignKeyAction::Restrict),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_sandbox_spawn_checkpoint_claim")
                .from_tbl(SandboxSpawnCheckpoint::Table)
                .from_col(SandboxSpawnCheckpoint::LeaseToken)
                .from_col(SandboxSpawnCheckpoint::OriginTurnId)
                .from_col(SandboxSpawnCheckpoint::AttemptCount)
                .from_col(SandboxSpawnCheckpoint::ClaimCount)
                .to_tbl(TurnClaim::Table)
                .to_col(TurnClaim::Token)
                .to_col(TurnClaim::TurnId)
                .to_col(TurnClaim::AttemptCount)
                .to_col(TurnClaim::ClaimCount)
                .on_delete(ForeignKeyAction::Restrict),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_sandbox_spawn_checkpoint_tool")
                .from_tbl(SandboxSpawnCheckpoint::Table)
                .from_col(SandboxSpawnCheckpoint::CallId)
                .from_col(SandboxSpawnCheckpoint::ChatId)
                .from_col(SandboxSpawnCheckpoint::HistoryOrder)
                .to_tbl(ToolCall::Table)
                .to_col(ToolCall::Id)
                .to_col(ToolCall::ChatId)
                .to_col(ToolCall::HistoryOrder)
                .on_delete(ForeignKeyAction::Restrict),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_sandbox_spawn_checkpoint_event")
                .from_tbl(SandboxSpawnCheckpoint::Table)
                .from_col(SandboxSpawnCheckpoint::ChatId)
                .from_col(SandboxSpawnCheckpoint::EventSeq)
                .to_tbl(Event::Table)
                .to_col(Event::ChatId)
                .to_col(Event::Seq)
                .on_delete(ForeignKeyAction::Restrict),
        )
        .check(Expr::col(SandboxSpawnCheckpoint::AttemptCount).gte(1))
        .check(
            Expr::col(SandboxSpawnCheckpoint::ClaimCount)
                .gte(Expr::col(SandboxSpawnCheckpoint::AttemptCount)),
        )
        .check(Expr::col(SandboxSpawnCheckpoint::SteerRevision).gte(0))
        .check(Expr::col(SandboxSpawnCheckpoint::EventOrdinal).between(2, i32::MAX - 1))
        // Zero is admissible: a spawn taken from a batch that was carried
        // across an earlier approval park is checkpointed by the claim that
        // resumed the turn, which reached the gate without calling the model.
        .check(Expr::col(SandboxSpawnCheckpoint::ModelSteps).gte(0))
        .check(Expr::col(SandboxSpawnCheckpoint::HistoryOrder).gt(0))
        .check(Expr::col(SandboxSpawnCheckpoint::InputTokens).between(0, i64::from(u32::MAX)))
        .check(Expr::col(SandboxSpawnCheckpoint::OutputTokens).between(0, i64::from(u32::MAX)))
        .check(
            Expr::col(SandboxSpawnCheckpoint::CacheReadInputTokens).between(0, i64::from(u32::MAX)),
        )
        .check(
            Expr::col(SandboxSpawnCheckpoint::CacheCreationInputTokens)
                .between(0, i64::from(u32::MAX)),
        )
        .check(
            Func::char_length(Expr::col(SandboxSpawnCheckpoint::ProviderId))
                .between(1, ToolCallRecord::MAX_LABEL_LEN as i32),
        )
        .check(
            Func::char_length(Expr::col(SandboxSpawnCheckpoint::Result))
                .lte(ToolCallRecord::MAX_RESULT_BYTES as i32),
        )
        .to_owned()
}

pub(super) fn sandbox_spawn_checkpoint_indexes() -> Vec<IndexCreateStatement> {
    vec![
        Index::create()
            .name("idx_sandbox_spawn_checkpoint_event")
            .table(SandboxSpawnCheckpoint::Table)
            .col(SandboxSpawnCheckpoint::ChatId)
            .col(SandboxSpawnCheckpoint::EventSeq)
            .unique()
            .to_owned(),
        // One claim segment can park on at most one spawn checkpoint — it
        // ends there — which is what lets the resuming claim find the batch
        // its predecessor left behind by claim identity alone.
        Index::create()
            .name("idx_sandbox_spawn_checkpoint_claim_segment")
            .table(SandboxSpawnCheckpoint::Table)
            .col(SandboxSpawnCheckpoint::OriginTurnId)
            .col(SandboxSpawnCheckpoint::AttemptCount)
            .col(SandboxSpawnCheckpoint::ClaimCount)
            .unique()
            .to_owned(),
    ]
}

/// One file an exec turn tried to change in a user folder.
///
/// Applied and rejected writes share one table because they are one journal:
/// the same identity, turn, folder, path, and retention window, differing only
/// in outcome. `classification` names that outcome and decides which columns
/// carry it — the applied side holds the blob with the file's prior contents,
/// which is what undo replays from; the rejected side holds only why the write
/// was left out. The per-classification CHECKs are what keep a row from
/// claiming both.
pub(super) fn exec_file_change_table() -> TableCreateStatement {
    Table::create()
        .table(ExecFileChange::Table)
        .col(
            ColumnDef::new(ExecFileChange::Id)
                .uuid()
                .not_null()
                .primary_key(),
        )
        .col(ColumnDef::new(ExecFileChange::ChatId).uuid().not_null())
        .col(ColumnDef::new(ExecFileChange::TurnId).uuid().not_null())
        .col(
            ColumnDef::new(ExecFileChange::Classification)
                .string_len(16)
                .not_null(),
        )
        .col(ColumnDef::new(ExecFileChange::FolderPath).text().not_null())
        .col(
            ColumnDef::new(ExecFileChange::RelativePath)
                .text()
                .not_null(),
        )
        .col(
            ColumnDef::new(ExecFileChange::ChangeKind)
                .string_len(32)
                .null(),
        )
        .col(ColumnDef::new(ExecFileChange::PriorBlobId).uuid().null())
        .col(
            ColumnDef::new(ExecFileChange::PriorByteLen)
                .big_integer()
                .null(),
        )
        .col(
            ColumnDef::new(ExecFileChange::NewSha256)
                .string_len(64)
                .null(),
        )
        .col(
            ColumnDef::new(ExecFileChange::UndoState)
                .string_len(32)
                .null(),
        )
        .col(ColumnDef::new(ExecFileChange::Reason).string_len(32).null())
        .col(
            ColumnDef::new(ExecFileChange::RecordedAt)
                .timestamp_with_time_zone()
                .not_null(),
        )
        // `Restrict`, not `Cascade`: a chat's prior-content blobs are live
        // references, so deleting the chat has to clear this journal
        // explicitly and retire what it freed rather than dropping the rows
        // out from under the blob store.
        .foreign_key(
            ForeignKey::create()
                .name("fk_exec_file_change_chat")
                .from(ExecFileChange::Table, ExecFileChange::ChatId)
                .to(Chat::Table, Chat::Id)
                .on_delete(ForeignKeyAction::Restrict),
        )
        .check(Expr::col(ExecFileChange::Classification).is_in([
            ExecFileChangeClassification::Applied.as_str(),
            ExecFileChangeClassification::Rejected.as_str(),
        ]))
        // Each classification carries its own outcome and nothing from the
        // other's: an applied change records what it did and whether it can be
        // undone, a rejected one records only why it was not applied.
        .check(
            Expr::col(ExecFileChange::Classification)
                .eq(ExecFileChangeClassification::Applied.as_str())
                .and(Expr::col(ExecFileChange::ChangeKind).is_not_null())
                .and(Expr::col(ExecFileChange::UndoState).is_not_null())
                .and(Expr::col(ExecFileChange::Reason).is_null())
                .or(Expr::col(ExecFileChange::Classification)
                    .eq(ExecFileChangeClassification::Rejected.as_str())
                    .and(Expr::col(ExecFileChange::Reason).is_not_null())
                    .and(Expr::col(ExecFileChange::ChangeKind).is_null())
                    .and(Expr::col(ExecFileChange::UndoState).is_null())
                    .and(Expr::col(ExecFileChange::PriorBlobId).is_null())
                    .and(Expr::col(ExecFileChange::PriorByteLen).is_null())
                    .and(Expr::col(ExecFileChange::NewSha256).is_null())),
        )
        .check(Expr::col(ExecFileChange::PriorBlobId).ne(uuid::Uuid::nil()))
        .check(Expr::col(ExecFileChange::PriorByteLen).gte(0))
        .check(Expr::col(ExecFileChange::ChangeKind).is_in([
            ExecFileChangeKind::Created.as_str(),
            ExecFileChangeKind::Overwritten.as_str(),
            ExecFileChangeKind::Deleted.as_str(),
        ]))
        .check(Expr::col(ExecFileChange::UndoState).is_in([
            ExecUndoState::Available.as_str(),
            ExecUndoState::PriorTooLarge.as_str(),
            ExecUndoState::PriorUnreadable.as_str(),
        ]))
        .check(Expr::col(ExecFileChange::Reason).is_in([
            ExecFileRejectionReason::Stale.as_str(),
            ExecFileRejectionReason::SnapshotUnavailable.as_str(),
            ExecFileRejectionReason::StagedFileTooLarge.as_str(),
            ExecFileRejectionReason::TrashUnavailable.as_str(),
            ExecFileRejectionReason::Unavailable.as_str(),
        ]))
        .to_owned()
}

pub(super) fn exec_file_change_indexes() -> Vec<IndexCreateStatement> {
    vec![
        Index::create()
            .name("idx_exec_file_change_blob")
            .table(ExecFileChange::Table)
            .col(ExecFileChange::PriorBlobId)
            .to_owned(),
        // Retention prunes the oldest turns of one chat, and undo reads the
        // newest; both walk this index rather than the whole journal.
        Index::create()
            .name("idx_exec_file_change_chat_turn")
            .table(ExecFileChange::Table)
            .col(ExecFileChange::ChatId)
            .col(ExecFileChange::RecordedAt)
            .col(ExecFileChange::TurnId)
            .to_owned(),
    ]
}

/// A background agent's durable task plan: one row per run, replaced whole by
/// each `update_task_plan` checkpoint that run resolves.
///
/// Keyed by the run rather than the chat, unlike the foreground `task_plan`.
/// One conversation can have several sandbox children working at once, and
/// each is doing a different delegated task; a chat-keyed row would make four
/// siblings overwrite each other's checklists and the conversation's own.
///
/// The steps ride as a JSON array for the same reasons the foreground table
/// does: they are bounded and validated at the tool boundary before anything
/// is written, nothing queries an individual step, and a whole-list
/// replacement is one row write rather than an ordered delete-and-reinsert.
///
/// `call_id` names the checkpoint whose resolution last wrote the row, which
/// is what lets a replayed resolution recognize its own write instead of
/// committing a second one.
pub(super) fn agent_run_task_plan_table() -> TableCreateStatement {
    Table::create()
        .table(AgentRunTaskPlan::Table)
        .col(
            ColumnDef::new(AgentRunTaskPlan::AgentRunId)
                .uuid()
                .not_null()
                .primary_key(),
        )
        .col(ColumnDef::new(AgentRunTaskPlan::CallId).uuid().not_null())
        .col(ColumnDef::new(AgentRunTaskPlan::Steps).text().not_null())
        .col(
            ColumnDef::new(AgentRunTaskPlan::CreatedAt)
                .timestamp_with_time_zone()
                .not_null(),
        )
        .col(
            ColumnDef::new(AgentRunTaskPlan::UpdatedAt)
                .timestamp_with_time_zone()
                .not_null(),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_agent_run_task_plan_run")
                .from(AgentRunTaskPlan::Table, AgentRunTaskPlan::AgentRunId)
                .to(AgentRun::Table, AgentRun::Id)
                .on_delete(ForeignKeyAction::Restrict),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_agent_run_task_plan_call")
                .from(AgentRunTaskPlan::Table, AgentRunTaskPlan::CallId)
                .to(SandboxToolCall::Table, SandboxToolCall::Id)
                .on_delete(ForeignKeyAction::Restrict),
        )
        .check(Expr::col(AgentRunTaskPlan::UpdatedAt).gte(Expr::col(AgentRunTaskPlan::CreatedAt)))
        .to_owned()
}

pub(super) fn agent_run_task_plan_indexes() -> Vec<IndexCreateStatement> {
    Vec::new()
}
