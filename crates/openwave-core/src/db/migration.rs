use sea_orm_migration::prelude::*;

use super::{
    BlobRetirementStatus, DocumentJobKind, DocumentJobStatus, DocumentProcessingStatus,
    TurnRunStatus, TurnSteerStatus,
};

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(Init),
            Box::new(AddEventJournal),
            Box::new(AddProjects),
            Box::new(AddChatModel),
            Box::new(AddToolCalls),
            Box::new(AddDocuments),
        ]
    }
}

struct Init;

impl MigrationName for Init {
    fn name(&self) -> &str {
        "m0001_init"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Init {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Chat::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Chat::Id).uuid().not_null().primary_key())
                    .col(ColumnDef::new(Chat::Title).text())
                    .col(ColumnDef::new(Chat::WorkspaceDir).text().not_null())
                    .col(
                        ColumnDef::new(Chat::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(MessageIdentity::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(MessageIdentity::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(MessageIdentity::ChatId).uuid().not_null())
                    .col(ColumnDef::new(MessageIdentity::TurnId).uuid().not_null())
                    .col(
                        ColumnDef::new(MessageIdentity::Owner)
                            .string_len(16)
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_message_identity_chat")
                            .from(MessageIdentity::Table, MessageIdentity::ChatId)
                            .to(Chat::Table, Chat::Id),
                    )
                    .check(Expr::col(MessageIdentity::Owner).is_in(["message", "turn_steer"]))
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(Message::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Message::Id).uuid().not_null().primary_key())
                    .col(ColumnDef::new(Message::ChatId).uuid().not_null())
                    .col(ColumnDef::new(Message::TurnId).uuid().not_null())
                    .col(ColumnDef::new(Message::Seq).big_integer().not_null())
                    .col(ColumnDef::new(Message::Role).text().not_null())
                    .col(ColumnDef::new(Message::Content).text().not_null())
                    .col(
                        ColumnDef::new(Message::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_message_chat")
                            .from(Message::Table, Message::ChatId)
                            .to(Chat::Table, Chat::Id),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_message_chat")
                    .table(Message::Table)
                    .col(Message::ChatId)
                    .col(Message::Seq)
                    .unique()
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_message_turn_identity")
                    .table(Message::Table)
                    .col(Message::Id)
                    .col(Message::ChatId)
                    .col(Message::TurnId)
                    .unique()
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(TurnClaim::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(TurnClaim::Token)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(TurnClaim::TurnId).uuid().not_null())
                    .col(ColumnDef::new(TurnClaim::AttemptCount).integer().not_null())
                    .col(
                        ColumnDef::new(TurnClaim::ClaimedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(TurnClaim::LeaseExpiresAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .check(Expr::col(TurnClaim::AttemptCount).gte(1))
                    .check(Expr::col(TurnClaim::LeaseExpiresAt).gt(Expr::col(TurnClaim::ClaimedAt)))
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_turn_claim_identity")
                    .table(TurnClaim::Table)
                    .col(TurnClaim::Token)
                    .col(TurnClaim::TurnId)
                    .col(TurnClaim::AttemptCount)
                    .unique()
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_turn_claim_attempt")
                    .table(TurnClaim::Table)
                    .col(TurnClaim::TurnId)
                    .col(TurnClaim::AttemptCount)
                    .unique()
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_turn_claim_turn_token")
                    .table(TurnClaim::Table)
                    .col(TurnClaim::TurnId)
                    .col(TurnClaim::Token)
                    .unique()
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(TurnClaimLock::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(TurnClaimLock::Id)
                            .integer()
                            .not_null()
                            .primary_key(),
                    )
                    .check(Expr::col(TurnClaimLock::Id).eq(1))
                    .to_owned(),
            )
            .await?;
        manager
            .get_connection()
            .execute_unprepared("INSERT INTO turn_claim_lock (id) VALUES (1)")
            .await?;

        let valid_turn_status = Expr::col(TurnRun::Status).is_in([
            TurnRunStatus::Queued.as_str(),
            TurnRunStatus::Running.as_str(),
            TurnRunStatus::Cancelling.as_str(),
            TurnRunStatus::RetryWait.as_str(),
            TurnRunStatus::Completed.as_str(),
            TurnRunStatus::Failed.as_str(),
            TurnRunStatus::Cancelled.as_str(),
        ]);
        let active_lease = Expr::col(TurnRun::Status)
            .is_in([
                TurnRunStatus::Running.as_str(),
                TurnRunStatus::Cancelling.as_str(),
            ])
            .and(Expr::col(TurnRun::LeaseToken).is_not_null())
            .and(Expr::col(TurnRun::LeaseExpiresAt).is_not_null());
        let no_lease = Expr::col(TurnRun::Status)
            .ne(TurnRunStatus::Running.as_str())
            .and(Expr::col(TurnRun::Status).ne(TurnRunStatus::Cancelling.as_str()))
            .and(Expr::col(TurnRun::LeaseToken).is_null())
            .and(Expr::col(TurnRun::LeaseExpiresAt).is_null());
        let completed_output = Expr::col(TurnRun::Status)
            .eq(TurnRunStatus::Completed.as_str())
            .and(Expr::col(TurnRun::OutputMessageId).is_not_null());
        let no_output = Expr::col(TurnRun::Status)
            .ne(TurnRunStatus::Completed.as_str())
            .and(Expr::col(TurnRun::OutputMessageId).is_null());
        let terminal_finished = Expr::col(TurnRun::Status)
            .is_in([
                TurnRunStatus::Completed.as_str(),
                TurnRunStatus::Failed.as_str(),
                TurnRunStatus::Cancelled.as_str(),
            ])
            .and(Expr::col(TurnRun::FinishedAt).is_not_null());
        let nonterminal_unfinished = Expr::col(TurnRun::Status)
            .is_in([
                TurnRunStatus::Queued.as_str(),
                TurnRunStatus::Running.as_str(),
                TurnRunStatus::Cancelling.as_str(),
                TurnRunStatus::RetryWait.as_str(),
            ])
            .and(Expr::col(TurnRun::FinishedAt).is_null());
        let queued_attempt = Expr::col(TurnRun::Status)
            .eq(TurnRunStatus::Queued.as_str())
            .and(Expr::col(TurnRun::AttemptCount).eq(0))
            .and(Expr::col(TurnRun::StartedAt).is_null());
        let leased_attempt = Expr::col(TurnRun::Status)
            .is_in([
                TurnRunStatus::Running.as_str(),
                TurnRunStatus::Cancelling.as_str(),
            ])
            .and(Expr::col(TurnRun::AttemptCount).gte(1))
            .and(Expr::col(TurnRun::StartedAt).is_not_null());
        let retryable_attempt = Expr::col(TurnRun::Status)
            .eq(TurnRunStatus::RetryWait.as_str())
            .and(Expr::col(TurnRun::AttemptCount).gte(1))
            .and(Expr::col(TurnRun::AttemptCount).lt(Expr::col(TurnRun::MaxAttempts)))
            .and(Expr::col(TurnRun::StartedAt).is_not_null());
        let resolved_attempt = Expr::col(TurnRun::Status)
            .is_in([
                TurnRunStatus::Completed.as_str(),
                TurnRunStatus::Failed.as_str(),
            ])
            .and(Expr::col(TurnRun::AttemptCount).gte(1))
            .and(Expr::col(TurnRun::StartedAt).is_not_null());
        let cancelled_attempt = Expr::col(TurnRun::Status)
            .eq(TurnRunStatus::Cancelled.as_str())
            .and(
                Expr::col(TurnRun::AttemptCount)
                    .eq(0)
                    .and(Expr::col(TurnRun::StartedAt).is_null())
                    .or(Expr::col(TurnRun::AttemptCount)
                        .gte(1)
                        .and(Expr::col(TurnRun::StartedAt).is_not_null())),
            );
        let failure_has_error = Expr::col(TurnRun::Status)
            .is_in([
                TurnRunStatus::RetryWait.as_str(),
                TurnRunStatus::Failed.as_str(),
            ])
            .and(Expr::col(TurnRun::LastErrorCode).is_not_null());
        let success_has_no_error = Expr::col(TurnRun::Status)
            .is_in([
                TurnRunStatus::Queued.as_str(),
                TurnRunStatus::Running.as_str(),
                TurnRunStatus::Cancelling.as_str(),
                TurnRunStatus::Completed.as_str(),
                TurnRunStatus::Cancelled.as_str(),
            ])
            .and(Expr::col(TurnRun::LastErrorCode).is_null())
            .and(Expr::col(TurnRun::LastErrorDetail).is_null());
        let coherent_steer_generation = Expr::col(TurnRun::SteerRevision)
            .eq(0)
            .and(Expr::col(TurnRun::LastSteerAppliedAt).is_null())
            .or(Expr::col(TurnRun::SteerRevision)
                .gte(1)
                .and(Expr::col(TurnRun::LastSteerAppliedAt).is_not_null()));

        manager
            .create_table(
                Table::create()
                    .table(TurnRun::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(TurnRun::Id).uuid().not_null().primary_key())
                    .col(ColumnDef::new(TurnRun::ChatId).uuid().not_null())
                    .col(ColumnDef::new(TurnRun::InputMessageId).uuid().not_null())
                    .col(ColumnDef::new(TurnRun::OutputMessageId).uuid())
                    .col(
                        ColumnDef::new(TurnRun::Model)
                            .string_len(crate::model::TurnRun::MAX_MODEL_LEN as u32)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(TurnRun::Status)
                            .string_len(32)
                            .not_null()
                            .default(TurnRunStatus::Queued.as_str()),
                    )
                    .col(
                        ColumnDef::new(TurnRun::AttemptCount)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(TurnRun::MaxAttempts)
                            .integer()
                            .not_null()
                            .default(crate::model::TurnRun::DEFAULT_MAX_ATTEMPTS),
                    )
                    .col(
                        ColumnDef::new(TurnRun::AvailableAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(ColumnDef::new(TurnRun::LeaseToken).uuid())
                    .col(ColumnDef::new(TurnRun::LeaseExpiresAt).timestamp_with_time_zone())
                    .col(ColumnDef::new(TurnRun::StartedAt).timestamp_with_time_zone())
                    .col(ColumnDef::new(TurnRun::FinishedAt).timestamp_with_time_zone())
                    .col(
                        ColumnDef::new(TurnRun::LastErrorCode)
                            .string_len(crate::model::TurnRun::MAX_ERROR_CODE_LEN as u32),
                    )
                    .col(
                        ColumnDef::new(TurnRun::LastErrorDetail)
                            .string_len(crate::model::TurnRun::MAX_ERROR_DETAIL_LEN as u32),
                    )
                    .col(
                        ColumnDef::new(TurnRun::SteerRevision)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .col(ColumnDef::new(TurnRun::LastSteerAppliedAt).timestamp_with_time_zone())
                    .col(
                        ColumnDef::new(TurnRun::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(TurnRun::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_turn_run_chat")
                            .from(TurnRun::Table, TurnRun::ChatId)
                            .to(Chat::Table, Chat::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_turn_run_input_message")
                            .from_tbl(TurnRun::Table)
                            .from_col(TurnRun::InputMessageId)
                            .from_col(TurnRun::ChatId)
                            .from_col(TurnRun::Id)
                            .to_tbl(Message::Table)
                            .to_col(Message::Id)
                            .to_col(Message::ChatId)
                            .to_col(Message::TurnId)
                            .on_delete(ForeignKeyAction::Restrict),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_turn_run_output_message")
                            .from_tbl(TurnRun::Table)
                            .from_col(TurnRun::OutputMessageId)
                            .from_col(TurnRun::ChatId)
                            .from_col(TurnRun::Id)
                            .to_tbl(Message::Table)
                            .to_col(Message::Id)
                            .to_col(Message::ChatId)
                            .to_col(Message::TurnId)
                            .on_delete(ForeignKeyAction::Restrict),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_turn_run_live_claim")
                            .from_tbl(TurnRun::Table)
                            .from_col(TurnRun::LeaseToken)
                            .from_col(TurnRun::Id)
                            .from_col(TurnRun::AttemptCount)
                            .to_tbl(TurnClaim::Table)
                            .to_col(TurnClaim::Token)
                            .to_col(TurnClaim::TurnId)
                            .to_col(TurnClaim::AttemptCount)
                            .on_delete(ForeignKeyAction::Restrict),
                    )
                    .check(
                        Func::char_length(Expr::col(TurnRun::Model))
                            .between(1, crate::model::TurnRun::MAX_MODEL_LEN as i32),
                    )
                    .check(valid_turn_status)
                    .check(
                        Expr::col(TurnRun::AttemptCount)
                            .gte(0)
                            .and(Expr::col(TurnRun::MaxAttempts).gte(1))
                            .and(
                                Expr::col(TurnRun::AttemptCount)
                                    .lte(Expr::col(TurnRun::MaxAttempts)),
                            ),
                    )
                    .check(active_lease.or(no_lease))
                    .check(completed_output.or(no_output))
                    .check(terminal_finished.or(nonterminal_unfinished))
                    .check(
                        queued_attempt
                            .or(leased_attempt)
                            .or(retryable_attempt)
                            .or(resolved_attempt)
                            .or(cancelled_attempt),
                    )
                    .check(failure_has_error.or(success_has_no_error))
                    .check(
                        Expr::col(TurnRun::LastErrorCode)
                            .is_null()
                            .or(Func::char_length(Expr::col(TurnRun::LastErrorCode))
                                .between(1, crate::model::TurnRun::MAX_ERROR_CODE_LEN as i32)),
                    )
                    .check(
                        Expr::col(TurnRun::LastErrorDetail)
                            .is_null()
                            .or(Func::char_length(Expr::col(TurnRun::LastErrorDetail))
                                .between(1, crate::model::TurnRun::MAX_ERROR_DETAIL_LEN as i32)),
                    )
                    .check(Expr::col(TurnRun::SteerRevision).gte(0))
                    .check(coherent_steer_generation)
                    .check(
                        Expr::col(TurnRun::LastSteerAppliedAt)
                            .is_null()
                            .or(Expr::col(TurnRun::LastSteerAppliedAt)
                                .gte(Expr::col(TurnRun::CreatedAt))
                                .and(
                                    Expr::col(TurnRun::LastSteerAppliedAt)
                                        .lte(Expr::col(TurnRun::UpdatedAt)),
                                )),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_turn_run_chat_identity")
                    .table(TurnRun::Table)
                    .col(TurnRun::ChatId)
                    .col(TurnRun::Id)
                    .unique()
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_turn_run_input_message")
                    .table(TurnRun::Table)
                    .col(TurnRun::InputMessageId)
                    .unique()
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_turn_run_one_active")
                    .table(TurnRun::Table)
                    .col(TurnRun::ChatId)
                    .unique()
                    .and_where(Expr::col(TurnRun::Status).is_in([
                        TurnRunStatus::Queued.as_str(),
                        TurnRunStatus::Running.as_str(),
                        TurnRunStatus::Cancelling.as_str(),
                        TurnRunStatus::RetryWait.as_str(),
                    ]))
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_turn_run_due")
                    .table(TurnRun::Table)
                    .col(TurnRun::Status)
                    .col(TurnRun::AvailableAt)
                    .col(TurnRun::CreatedAt)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_turn_run_lease_token")
                    .table(TurnRun::Table)
                    .col(TurnRun::LeaseToken)
                    .unique()
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_turn_run_stale_lease")
                    .table(TurnRun::Table)
                    .col(TurnRun::Status)
                    .col(TurnRun::LeaseExpiresAt)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_turn_run_history")
                    .table(TurnRun::Table)
                    .col(TurnRun::ChatId)
                    .col(TurnRun::CreatedAt)
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(TurnFailure::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(TurnFailure::LeaseToken)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(TurnFailure::TurnId).uuid().not_null())
                    .col(
                        ColumnDef::new(TurnFailure::AttemptCount)
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(TurnFailure::RequestedRetryAt).timestamp_with_time_zone())
                    .col(
                        ColumnDef::new(TurnFailure::ErrorCode)
                            .string_len(crate::model::TurnRun::MAX_ERROR_CODE_LEN as u32)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(TurnFailure::ErrorDetail)
                            .string_len(crate::model::TurnRun::MAX_ERROR_DETAIL_LEN as u32),
                    )
                    .col(
                        ColumnDef::new(TurnFailure::ResolvedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(TurnFailure::ResultStatus)
                            .string_len(32)
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_turn_failure_claim")
                            .from_tbl(TurnFailure::Table)
                            .from_col(TurnFailure::LeaseToken)
                            .from_col(TurnFailure::TurnId)
                            .from_col(TurnFailure::AttemptCount)
                            .to_tbl(TurnClaim::Table)
                            .to_col(TurnClaim::Token)
                            .to_col(TurnClaim::TurnId)
                            .to_col(TurnClaim::AttemptCount)
                            .on_delete(ForeignKeyAction::Restrict),
                    )
                    .check(Expr::col(TurnFailure::AttemptCount).gte(1))
                    .check(Expr::col(TurnFailure::ResultStatus).is_in([
                        TurnRunStatus::RetryWait.as_str(),
                        TurnRunStatus::Failed.as_str(),
                    ]))
                    .check(
                        Expr::col(TurnFailure::ResultStatus)
                            .ne(TurnRunStatus::RetryWait.as_str())
                            .or(Expr::col(TurnFailure::RequestedRetryAt).is_not_null()),
                    )
                    .check(
                        Expr::col(TurnFailure::RequestedRetryAt)
                            .is_null()
                            .or(Expr::col(TurnFailure::RequestedRetryAt)
                                .gt(Expr::col(TurnFailure::ResolvedAt))),
                    )
                    .check(
                        Func::char_length(Expr::col(TurnFailure::ErrorCode))
                            .between(1, crate::model::TurnRun::MAX_ERROR_CODE_LEN as i32),
                    )
                    .check(
                        Expr::col(TurnFailure::ErrorDetail)
                            .is_null()
                            .or(Func::char_length(Expr::col(TurnFailure::ErrorDetail))
                                .between(1, crate::model::TurnRun::MAX_ERROR_DETAIL_LEN as i32)),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(Setting::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Setting::Key).text().not_null().primary_key())
                    .col(ColumnDef::new(Setting::ValueJson).json_binary().not_null())
                    .to_owned(),
            )
            .await?;

        let pending_steer = Expr::col(TurnSteer::Status)
            .eq(TurnSteerStatus::Pending.as_str())
            .and(Expr::col(TurnSteer::AppliedLeaseToken).is_null())
            .and(Expr::col(TurnSteer::MessageId).is_null())
            .and(Expr::col(TurnSteer::ResolvedAt).is_null());
        let applied_steer = Expr::col(TurnSteer::Status)
            .eq(TurnSteerStatus::Applied.as_str())
            .and(Expr::col(TurnSteer::AppliedLeaseToken).is_not_null())
            .and(Expr::col(TurnSteer::MessageId).is_not_null())
            .and(Expr::col(TurnSteer::ResolvedAt).is_not_null());
        let rejected_steer = Expr::col(TurnSteer::Status)
            .eq(TurnSteerStatus::Rejected.as_str())
            .and(Expr::col(TurnSteer::AppliedLeaseToken).is_null())
            .and(Expr::col(TurnSteer::MessageId).is_null())
            .and(Expr::col(TurnSteer::ResolvedAt).is_not_null());
        manager
            .create_table(
                Table::create()
                    .table(TurnSteer::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(TurnSteer::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(TurnSteer::TurnId).uuid().not_null())
                    .col(ColumnDef::new(TurnSteer::ChatId).uuid().not_null())
                    .col(ColumnDef::new(TurnSteer::Content).text().not_null())
                    .col(
                        ColumnDef::new(TurnSteer::Interrupt)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(TurnSteer::Status)
                            .string_len(16)
                            .not_null()
                            .default(TurnSteerStatus::Pending.as_str()),
                    )
                    .col(ColumnDef::new(TurnSteer::AppliedLeaseToken).uuid())
                    .col(ColumnDef::new(TurnSteer::MessageId).uuid())
                    .col(
                        ColumnDef::new(TurnSteer::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(ColumnDef::new(TurnSteer::ResolvedAt).timestamp_with_time_zone())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_turn_steer_turn")
                            .from_tbl(TurnSteer::Table)
                            .from_col(TurnSteer::ChatId)
                            .from_col(TurnSteer::TurnId)
                            .to_tbl(TurnRun::Table)
                            .to_col(TurnRun::ChatId)
                            .to_col(TurnRun::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_turn_steer_claim")
                            .from_tbl(TurnSteer::Table)
                            .from_col(TurnSteer::AppliedLeaseToken)
                            .from_col(TurnSteer::TurnId)
                            .to_tbl(TurnClaim::Table)
                            .to_col(TurnClaim::Token)
                            .to_col(TurnClaim::TurnId)
                            .on_delete(ForeignKeyAction::Restrict),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_turn_steer_message")
                            .from_tbl(TurnSteer::Table)
                            .from_col(TurnSteer::MessageId)
                            .from_col(TurnSteer::ChatId)
                            .from_col(TurnSteer::TurnId)
                            .to_tbl(Message::Table)
                            .to_col(Message::Id)
                            .to_col(Message::ChatId)
                            .to_col(Message::TurnId)
                            .on_delete(ForeignKeyAction::Restrict),
                    )
                    .check(
                        Func::char_length(Expr::col(TurnSteer::Content))
                            .between(1, crate::model::TurnSteer::MAX_CONTENT_LEN as i32),
                    )
                    .check(pending_steer.or(applied_steer).or(rejected_steer))
                    .check(
                        Expr::col(TurnSteer::MessageId)
                            .is_null()
                            .or(Expr::col(TurnSteer::MessageId).eq(Expr::col(TurnSteer::Id))),
                    )
                    .check(
                        Expr::col(TurnSteer::ResolvedAt)
                            .is_null()
                            .or(Expr::col(TurnSteer::ResolvedAt)
                                .gte(Expr::col(TurnSteer::CreatedAt))),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_turn_steer_pending")
                    .table(TurnSteer::Table)
                    .col(TurnSteer::TurnId)
                    .col(TurnSteer::Status)
                    .col(TurnSteer::CreatedAt)
                    .col(TurnSteer::Id)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_turn_steer_message")
                    .table(TurnSteer::Table)
                    .col(TurnSteer::MessageId)
                    .unique()
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(TurnSteer::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(TurnFailure::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(TurnRun::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Setting::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Message::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(MessageIdentity::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(TurnClaim::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(TurnClaimLock::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Chat::Table).to_owned())
            .await?;
        Ok(())
    }
}

/// Adds the per-chat event journal that clients replay from on connect.
struct AddEventJournal;

impl MigrationName for AddEventJournal {
    fn name(&self) -> &str {
        "m0002_event_journal"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AddEventJournal {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Event::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Event::ChatId).uuid().not_null())
                    .col(ColumnDef::new(Event::Seq).big_integer().not_null())
                    .col(ColumnDef::new(Event::TurnId).uuid())
                    .col(ColumnDef::new(Event::LeaseToken).uuid())
                    .col(ColumnDef::new(Event::AttemptEventOrdinal).integer())
                    .col(ColumnDef::new(Event::ScanToken).uuid())
                    .col(
                        ColumnDef::new(Event::Terminal)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(ColumnDef::new(Event::Payload).json_binary().not_null())
                    .col(
                        ColumnDef::new(Event::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .primary_key(Index::create().col(Event::ChatId).col(Event::Seq))
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_event_chat")
                            .from(Event::Table, Event::ChatId)
                            .to(Chat::Table, Chat::Id),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_event_turn")
                            .from_tbl(Event::Table)
                            .from_col(Event::ChatId)
                            .from_col(Event::TurnId)
                            .to_tbl(TurnRun::Table)
                            .to_col(TurnRun::ChatId)
                            .to_col(TurnRun::Id)
                            .on_delete(ForeignKeyAction::Restrict),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_event_turn_claim")
                            .from_tbl(Event::Table)
                            .from_col(Event::TurnId)
                            .from_col(Event::LeaseToken)
                            .to_tbl(TurnClaim::Table)
                            .to_col(TurnClaim::TurnId)
                            .to_col(TurnClaim::Token)
                            .on_delete(ForeignKeyAction::Restrict),
                    )
                    .check(
                        Expr::col(Event::Terminal)
                            .eq(false)
                            .or(Expr::col(Event::TurnId).is_not_null()),
                    )
                    .check(
                        Expr::col(Event::LeaseToken)
                            .is_null()
                            .and(Expr::col(Event::AttemptEventOrdinal).is_null())
                            .or(Expr::col(Event::LeaseToken).is_not_null().and(
                                Expr::col(Event::AttemptEventOrdinal)
                                    .is_not_null()
                                    .and(Expr::col(Event::TurnId).is_not_null()),
                            )),
                    )
                    .check(
                        Expr::col(Event::AttemptEventOrdinal)
                            .is_null()
                            .or(Expr::col(Event::AttemptEventOrdinal).gte(1)),
                    )
                    .check(
                        Expr::col(Event::TurnId)
                            .is_null()
                            .or(Expr::col(Event::Terminal).eq(true))
                            .or(Expr::col(Event::LeaseToken).is_not_null()),
                    )
                    .check(
                        Expr::col(Event::ScanToken)
                            .is_null()
                            .or(Expr::col(Event::Terminal).eq(true)),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_event_attempt_ordinal")
                    .table(Event::Table)
                    .col(Event::LeaseToken)
                    .col(Event::AttemptEventOrdinal)
                    .unique()
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_event_scan_token")
                    .table(Event::Table)
                    .col(Event::ScanToken)
                    .unique()
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_event_one_terminal_per_turn")
                    .table(Event::Table)
                    .col(Event::TurnId)
                    .unique()
                    .and_where(Expr::col(Event::Terminal).eq(true))
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Event::Table).to_owned())
            .await?;
        Ok(())
    }
}

/// Adds the `project` table and the optional `chat.project_id` link.
struct AddProjects;

impl MigrationName for AddProjects {
    fn name(&self) -> &str {
        "m0003_projects"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AddProjects {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Project::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Project::Id).uuid().not_null().primary_key())
                    .col(ColumnDef::new(Project::Title).text())
                    .col(ColumnDef::new(Project::WorkspaceDir).text().not_null())
                    .col(
                        ColumnDef::new(Project::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        // A nullable link, no DB-level foreign key: SQLite can't add an FK to
        // an existing table, so membership is validated at the API edge (the
        // server checks the project exists before creating the chat).
        manager
            .alter_table(
                Table::alter()
                    .table(Chat::Table)
                    .add_column(ColumnDef::new(Chat::ProjectId).uuid())
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Chat::Table)
                    .drop_column(Chat::ProjectId)
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(Table::drop().table(Project::Table).to_owned())
            .await?;
        Ok(())
    }
}

/// Adds the optional per-chat `model` override.
struct AddChatModel;

impl MigrationName for AddChatModel {
    fn name(&self) -> &str {
        "m0004_chat_model"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AddChatModel {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Chat::Table)
                    .add_column(ColumnDef::new(Chat::Model).text())
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Chat::Table)
                    .drop_column(Chat::Model)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

/// Structured tool-call rows (args + result), distinct from text messages.
struct AddToolCalls;

impl MigrationName for AddToolCalls {
    fn name(&self) -> &str {
        "m0005_tool_calls"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AddToolCalls {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(ToolCall::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(ToolCall::Id).uuid().not_null().primary_key())
                    .col(ColumnDef::new(ToolCall::ChatId).uuid().not_null())
                    .col(ColumnDef::new(ToolCall::TurnId).uuid().not_null())
                    .col(ColumnDef::new(ToolCall::ProviderId).text().not_null())
                    .col(ColumnDef::new(ToolCall::Name).text().not_null())
                    .col(ColumnDef::new(ToolCall::Arguments).json_binary().not_null())
                    .col(ColumnDef::new(ToolCall::Result).text())
                    .col(
                        ColumnDef::new(ToolCall::IsError)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(ToolCall::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(ColumnDef::new(ToolCall::CompletedAt).timestamp_with_time_zone())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_tool_call_chat")
                            .from(ToolCall::Table, ToolCall::ChatId)
                            .to(Chat::Table, Chat::Id),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_tool_call_chat")
                    .table(ToolCall::Table)
                    .col(ToolCall::ChatId)
                    .col(ToolCall::CreatedAt)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(ToolCall::Table).to_owned())
            .await?;
        Ok(())
    }
}

/// Adds authoritative documents and their durable processing jobs. The
/// retrieval database remains derived state; lifecycle, retry, and lease
/// ownership live in the operational database.
struct AddDocuments;

impl MigrationName for AddDocuments {
    fn name(&self) -> &str {
        "m0006_document_catalog"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AddDocuments {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let valid_index_revision =
            Expr::col(Document::IndexedRevision)
                .is_null()
                .or(Expr::col(Document::IndexedRevision).gte(1).and(
                    Expr::col(Document::IndexedRevision).lte(Expr::col(Document::ContentRevision)),
                ));
        let watermark_absent = Expr::col(Document::IndexedRevision)
            .is_null()
            .and(Expr::col(Document::IndexFingerprint).is_null())
            .and(Expr::col(Document::IndexedAt).is_null());
        let watermark_present = Expr::col(Document::IndexedRevision)
            .is_not_null()
            .and(Expr::col(Document::IndexFingerprint).is_not_null().and(
                Func::char_length(Expr::col(Document::IndexFingerprint)).between(
                    1,
                    crate::model::DocumentJob::MAX_PIPELINE_FINGERPRINT_LEN as i32,
                ),
            ))
            .and(Expr::col(Document::IndexedAt).is_not_null());
        let processing_watermark_consistent = Expr::col(Document::ProcessingStatus)
            .eq(DocumentProcessingStatus::Ready.as_str())
            .and(Expr::col(Document::IndexedRevision).eq(Expr::col(Document::ContentRevision)))
            .and(watermark_present)
            .or(Expr::col(Document::ProcessingStatus)
                .ne(DocumentProcessingStatus::Ready.as_str())
                .and(watermark_absent));

        manager
            .create_table(
                Table::create()
                    .table(DocumentGeneration::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(DocumentGeneration::DocumentId)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(DocumentGeneration::ContentRevision)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(DocumentGeneration::RevisionToken)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(DocumentGeneration::Tombstone)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(DocumentGeneration::RetirementPending)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(DocumentGeneration::RetirementContentRevision).big_integer(),
                    )
                    .col(ColumnDef::new(DocumentGeneration::RetirementRevisionToken).uuid())
                    .check(Expr::col(DocumentGeneration::ContentRevision).gte(1))
                    .check(
                        Expr::col(DocumentGeneration::RetirementPending)
                            .eq(false)
                            .and(Expr::col(DocumentGeneration::RetirementContentRevision).is_null())
                            .and(Expr::col(DocumentGeneration::RetirementRevisionToken).is_null())
                            .or(Expr::col(DocumentGeneration::RetirementPending)
                                .eq(true)
                                .and(
                                    Expr::col(DocumentGeneration::RetirementContentRevision)
                                        .is_not_null(),
                                )
                                .and(
                                    Expr::col(DocumentGeneration::RetirementContentRevision).gte(1),
                                )
                                .and(
                                    Expr::col(DocumentGeneration::RetirementRevisionToken)
                                        .is_not_null(),
                                )),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(Document::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Document::Id).uuid().not_null().primary_key())
                    .col(ColumnDef::new(Document::ProjectId).uuid())
                    .col(ColumnDef::new(Document::SourceUri).text())
                    .col(ColumnDef::new(Document::MediaType).text().not_null())
                    .col(ColumnDef::new(Document::Title).text())
                    .col(ColumnDef::new(Document::SourceBlobId).uuid())
                    .col(ColumnDef::new(Document::SourceSha256).binary())
                    .col(ColumnDef::new(Document::SourceByteLen).big_integer())
                    .col(ColumnDef::new(Document::CanonicalText).text().not_null())
                    .col(ColumnDef::new(Document::CanonicalFingerprint).text())
                    .col(
                        ColumnDef::new(Document::SourceRegions)
                            .json_binary()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Document::ContentRevision)
                            .big_integer()
                            .not_null()
                            .default(1),
                    )
                    .col(ColumnDef::new(Document::RevisionToken).uuid().not_null())
                    .col(
                        ColumnDef::new(Document::ProcessingStatus)
                            .text()
                            .not_null()
                            .default(DocumentProcessingStatus::Queued.as_str()),
                    )
                    .col(ColumnDef::new(Document::IndexedRevision).big_integer())
                    .col(ColumnDef::new(Document::IndexFingerprint).text())
                    .col(
                        ColumnDef::new(Document::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Document::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Document::IndexedAt).timestamp_with_time_zone())
                    .check(
                        Expr::col(Document::SourceBlobId)
                            .is_null()
                            .and(Expr::col(Document::SourceSha256).is_null())
                            .and(Expr::col(Document::SourceByteLen).is_null())
                            .or(Expr::col(Document::SourceBlobId)
                                .is_not_null()
                                .and(Expr::col(Document::SourceSha256).is_not_null())
                                .and(Expr::col(Document::SourceByteLen).is_not_null())
                                .and(Expr::cust("LENGTH(source_sha256) = 32"))
                                .and(Expr::col(Document::SourceByteLen).gte(0))),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_document_project")
                            .from(Document::Table, Document::ProjectId)
                            .to(Project::Table, Project::Id)
                            .on_delete(ForeignKeyAction::Restrict),
                    )
                    .check(Expr::col(Document::MediaType).ne(""))
                    .check(
                        Expr::col(Document::SourceUri)
                            .is_null()
                            .or(Expr::col(Document::SourceUri).ne("")),
                    )
                    .check(Expr::col(Document::CanonicalFingerprint).is_null().or(
                        Func::char_length(Expr::col(Document::CanonicalFingerprint)).between(
                            1,
                            crate::model::DocumentJob::MAX_PIPELINE_FINGERPRINT_LEN as i32,
                        ),
                    ))
                    .check(Expr::col(Document::ContentRevision).gte(1))
                    .check(Expr::col(Document::ProcessingStatus).is_in([
                        DocumentProcessingStatus::Queued.as_str(),
                        DocumentProcessingStatus::Processing.as_str(),
                        DocumentProcessingStatus::Ready.as_str(),
                        DocumentProcessingStatus::Failed.as_str(),
                    ]))
                    .check(valid_index_revision)
                    .check(processing_watermark_consistent)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_document_project_created")
                    .table(Document::Table)
                    .col(Document::ProjectId)
                    .col(Document::CreatedAt)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_document_source_blob")
                    .table(Document::Table)
                    .col(Document::SourceBlobId)
                    .to_owned(),
            )
            .await?;

        let valid_blob_status = Expr::col(BlobRetirement::Status).is_in([
            BlobRetirementStatus::Queued.as_str(),
            BlobRetirementStatus::Running.as_str(),
            BlobRetirementStatus::RetryWait.as_str(),
            BlobRetirementStatus::Succeeded.as_str(),
            BlobRetirementStatus::Failed.as_str(),
            BlobRetirementStatus::Cancelled.as_str(),
        ]);
        let blob_running_lease = Expr::col(BlobRetirement::Status)
            .eq(BlobRetirementStatus::Running.as_str())
            .and(Expr::col(BlobRetirement::LeaseToken).is_not_null())
            .and(Expr::col(BlobRetirement::LeaseExpiresAt).is_not_null());
        let blob_no_lease = Expr::col(BlobRetirement::Status)
            .ne(BlobRetirementStatus::Running.as_str())
            .and(Expr::col(BlobRetirement::LeaseToken).is_null())
            .and(Expr::col(BlobRetirement::LeaseExpiresAt).is_null());
        let blob_terminal_finished = Expr::col(BlobRetirement::Status)
            .is_in([
                BlobRetirementStatus::Succeeded.as_str(),
                BlobRetirementStatus::Failed.as_str(),
                BlobRetirementStatus::Cancelled.as_str(),
            ])
            .and(Expr::col(BlobRetirement::FinishedAt).is_not_null());
        let blob_nonterminal_unfinished = Expr::col(BlobRetirement::Status)
            .is_in([
                BlobRetirementStatus::Queued.as_str(),
                BlobRetirementStatus::Running.as_str(),
                BlobRetirementStatus::RetryWait.as_str(),
            ])
            .and(Expr::col(BlobRetirement::FinishedAt).is_null());
        let blob_queued_attempt = Expr::col(BlobRetirement::Status)
            .eq(BlobRetirementStatus::Queued.as_str())
            .and(Expr::col(BlobRetirement::AttemptCount).eq(0))
            .and(Expr::col(BlobRetirement::StartedAt).is_null());
        let blob_running_attempt = Expr::col(BlobRetirement::Status)
            .eq(BlobRetirementStatus::Running.as_str())
            .and(Expr::col(BlobRetirement::AttemptCount).gte(1))
            .and(Expr::col(BlobRetirement::StartedAt).is_not_null());
        let blob_retryable_attempt = Expr::col(BlobRetirement::Status)
            .eq(BlobRetirementStatus::RetryWait.as_str())
            .and(Expr::col(BlobRetirement::AttemptCount).gte(1))
            .and(Expr::col(BlobRetirement::AttemptCount).lt(Expr::col(BlobRetirement::MaxAttempts)))
            .and(Expr::col(BlobRetirement::StartedAt).is_not_null());
        let blob_completed_attempt = Expr::col(BlobRetirement::Status)
            .is_in([
                BlobRetirementStatus::Succeeded.as_str(),
                BlobRetirementStatus::Failed.as_str(),
            ])
            .and(Expr::col(BlobRetirement::AttemptCount).gte(1))
            .and(Expr::col(BlobRetirement::StartedAt).is_not_null());
        let blob_cancelled_attempt = Expr::col(BlobRetirement::Status)
            .eq(BlobRetirementStatus::Cancelled.as_str())
            .and(
                Expr::col(BlobRetirement::AttemptCount)
                    .eq(0)
                    .and(Expr::col(BlobRetirement::StartedAt).is_null())
                    .or(Expr::col(BlobRetirement::AttemptCount)
                        .gte(1)
                        .and(Expr::col(BlobRetirement::StartedAt).is_not_null())),
            );
        manager
            .create_table(
                Table::create()
                    .table(BlobRetirement::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(BlobRetirement::BlobId)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(BlobRetirement::Status)
                            .string_len(32)
                            .not_null()
                            .default(BlobRetirementStatus::Queued.as_str()),
                    )
                    .col(
                        ColumnDef::new(BlobRetirement::AttemptCount)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(BlobRetirement::MaxAttempts)
                            .integer()
                            .not_null()
                            .default(crate::model::BlobRetirement::DEFAULT_MAX_ATTEMPTS),
                    )
                    .col(
                        ColumnDef::new(BlobRetirement::AvailableAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(ColumnDef::new(BlobRetirement::LeaseToken).uuid())
                    .col(ColumnDef::new(BlobRetirement::LeaseExpiresAt).timestamp_with_time_zone())
                    .col(ColumnDef::new(BlobRetirement::StartedAt).timestamp_with_time_zone())
                    .col(ColumnDef::new(BlobRetirement::FinishedAt).timestamp_with_time_zone())
                    .col(
                        ColumnDef::new(BlobRetirement::LastErrorCode)
                            .string_len(crate::model::BlobRetirement::MAX_ERROR_CODE_LEN as u32),
                    )
                    .col(
                        ColumnDef::new(BlobRetirement::LastErrorDetail)
                            .string_len(crate::model::BlobRetirement::MAX_ERROR_DETAIL_LEN as u32),
                    )
                    .col(
                        ColumnDef::new(BlobRetirement::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(BlobRetirement::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .check(valid_blob_status)
                    .check(
                        Expr::col(BlobRetirement::AttemptCount)
                            .gte(0)
                            .and(Expr::col(BlobRetirement::MaxAttempts).gte(1))
                            .and(
                                Expr::col(BlobRetirement::AttemptCount)
                                    .lte(Expr::col(BlobRetirement::MaxAttempts)),
                            ),
                    )
                    .check(blob_running_lease.or(blob_no_lease))
                    .check(blob_terminal_finished.or(blob_nonterminal_unfinished))
                    .check(
                        blob_queued_attempt
                            .or(blob_running_attempt)
                            .or(blob_retryable_attempt)
                            .or(blob_completed_attempt)
                            .or(blob_cancelled_attempt),
                    )
                    .check(
                        Expr::col(BlobRetirement::LastErrorCode)
                            .is_null()
                            .or(Func::char_length(Expr::col(BlobRetirement::LastErrorCode))
                                .between(
                                    1,
                                    crate::model::BlobRetirement::MAX_ERROR_CODE_LEN as i32,
                                )),
                    )
                    .check(
                        Expr::col(BlobRetirement::LastErrorDetail)
                            .is_null()
                            .or(
                                Func::char_length(Expr::col(BlobRetirement::LastErrorDetail))
                                    .between(
                                        1,
                                        crate::model::BlobRetirement::MAX_ERROR_DETAIL_LEN as i32,
                                    ),
                            ),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_blob_retirement_due")
                    .table(BlobRetirement::Table)
                    .col(BlobRetirement::Status)
                    .col(BlobRetirement::AvailableAt)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_blob_retirement_stale_lease")
                    .table(BlobRetirement::Table)
                    .col(BlobRetirement::Status)
                    .col(BlobRetirement::LeaseExpiresAt)
                    .to_owned(),
            )
            .await?;

        let valid_job_status = Expr::col(DocumentJob::Status).is_in([
            DocumentJobStatus::Queued.as_str(),
            DocumentJobStatus::Running.as_str(),
            DocumentJobStatus::RetryWait.as_str(),
            DocumentJobStatus::Succeeded.as_str(),
            DocumentJobStatus::Failed.as_str(),
            DocumentJobStatus::Cancelled.as_str(),
        ]);
        let running_lease = Expr::col(DocumentJob::Status)
            .eq(DocumentJobStatus::Running.as_str())
            .and(Expr::col(DocumentJob::LeaseToken).is_not_null())
            .and(Expr::col(DocumentJob::LeaseExpiresAt).is_not_null());
        let no_lease = Expr::col(DocumentJob::Status)
            .ne(DocumentJobStatus::Running.as_str())
            .and(Expr::col(DocumentJob::LeaseToken).is_null())
            .and(Expr::col(DocumentJob::LeaseExpiresAt).is_null());
        let terminal_finished = Expr::col(DocumentJob::Status)
            .is_in([
                DocumentJobStatus::Succeeded.as_str(),
                DocumentJobStatus::Failed.as_str(),
                DocumentJobStatus::Cancelled.as_str(),
            ])
            .and(Expr::col(DocumentJob::FinishedAt).is_not_null());
        let nonterminal_unfinished = Expr::col(DocumentJob::Status)
            .is_in([
                DocumentJobStatus::Queued.as_str(),
                DocumentJobStatus::Running.as_str(),
                DocumentJobStatus::RetryWait.as_str(),
            ])
            .and(Expr::col(DocumentJob::FinishedAt).is_null());
        let queued_attempt = Expr::col(DocumentJob::Status)
            .eq(DocumentJobStatus::Queued.as_str())
            .and(Expr::col(DocumentJob::AttemptCount).eq(0))
            .and(Expr::col(DocumentJob::StartedAt).is_null());
        let running_attempt = Expr::col(DocumentJob::Status)
            .eq(DocumentJobStatus::Running.as_str())
            .and(Expr::col(DocumentJob::AttemptCount).gte(1))
            .and(Expr::col(DocumentJob::StartedAt).is_not_null());
        let retryable_attempt = Expr::col(DocumentJob::Status)
            .eq(DocumentJobStatus::RetryWait.as_str())
            .and(Expr::col(DocumentJob::AttemptCount).gte(1))
            .and(Expr::col(DocumentJob::AttemptCount).lt(Expr::col(DocumentJob::MaxAttempts)))
            .and(Expr::col(DocumentJob::StartedAt).is_not_null());
        let completed_attempt = Expr::col(DocumentJob::Status)
            .is_in([
                DocumentJobStatus::Succeeded.as_str(),
                DocumentJobStatus::Failed.as_str(),
            ])
            .and(Expr::col(DocumentJob::AttemptCount).gte(1))
            .and(Expr::col(DocumentJob::StartedAt).is_not_null());
        let cancelled_attempt = Expr::col(DocumentJob::Status)
            .eq(DocumentJobStatus::Cancelled.as_str())
            .and(
                Expr::col(DocumentJob::AttemptCount)
                    .eq(0)
                    .and(Expr::col(DocumentJob::StartedAt).is_null())
                    .or(Expr::col(DocumentJob::AttemptCount)
                        .gte(1)
                        .and(Expr::col(DocumentJob::StartedAt).is_not_null())),
            );

        manager
            .create_table(
                Table::create()
                    .table(DocumentJob::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(DocumentJob::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(DocumentJob::DocumentId).uuid().not_null())
                    .col(
                        ColumnDef::new(DocumentJob::ContentRevision)
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(DocumentJob::RevisionToken).uuid().not_null())
                    .col(ColumnDef::new(DocumentJob::Kind).string_len(64).not_null())
                    .col(
                        ColumnDef::new(DocumentJob::Status)
                            .string_len(32)
                            .not_null()
                            .default(DocumentJobStatus::Queued.as_str()),
                    )
                    .col(
                        ColumnDef::new(DocumentJob::PipelineFingerprint)
                            .string_len(512)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(DocumentJob::AttemptCount)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(DocumentJob::MaxAttempts)
                            .integer()
                            .not_null()
                            .default(5),
                    )
                    .col(
                        ColumnDef::new(DocumentJob::AvailableAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(ColumnDef::new(DocumentJob::LeaseToken).uuid())
                    .col(ColumnDef::new(DocumentJob::LeaseExpiresAt).timestamp_with_time_zone())
                    .col(ColumnDef::new(DocumentJob::StartedAt).timestamp_with_time_zone())
                    .col(ColumnDef::new(DocumentJob::FinishedAt).timestamp_with_time_zone())
                    .col(
                        ColumnDef::new(DocumentJob::LastErrorCode)
                            .string_len(crate::model::DocumentJob::MAX_ERROR_CODE_LEN as u32),
                    )
                    .col(
                        ColumnDef::new(DocumentJob::LastErrorDetail)
                            .string_len(crate::model::DocumentJob::MAX_ERROR_DETAIL_LEN as u32),
                    )
                    .col(
                        ColumnDef::new(DocumentJob::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(DocumentJob::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_document_job_document")
                            .from(DocumentJob::Table, DocumentJob::DocumentId)
                            .to(Document::Table, Document::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .check(Expr::col(DocumentJob::ContentRevision).gte(1))
                    .check(Expr::col(DocumentJob::Kind).is_in([
                        DocumentJobKind::Parse.as_str(),
                        DocumentJobKind::Index.as_str(),
                    ]))
                    .check(
                        Func::char_length(Expr::col(DocumentJob::Kind))
                            .lte(64)
                            .and(
                                Func::char_length(Expr::col(DocumentJob::PipelineFingerprint))
                                    .between(
                                        1,
                                        crate::model::DocumentJob::MAX_PIPELINE_FINGERPRINT_LEN
                                            as i32,
                                    ),
                            )
                            .and(Expr::col(DocumentJob::LastErrorCode).is_null().or(
                                Func::char_length(Expr::col(DocumentJob::LastErrorCode)).between(
                                    1,
                                    crate::model::DocumentJob::MAX_ERROR_CODE_LEN as i32,
                                ),
                            ))
                            .and(Expr::col(DocumentJob::LastErrorDetail).is_null().or(
                                Func::char_length(Expr::col(DocumentJob::LastErrorDetail)).between(
                                    1,
                                    crate::model::DocumentJob::MAX_ERROR_DETAIL_LEN as i32,
                                ),
                            )),
                    )
                    .check(valid_job_status)
                    .check(
                        Expr::col(DocumentJob::AttemptCount)
                            .gte(0)
                            .and(Expr::col(DocumentJob::MaxAttempts).gte(1))
                            .and(
                                Expr::col(DocumentJob::AttemptCount)
                                    .lte(Expr::col(DocumentJob::MaxAttempts)),
                            ),
                    )
                    .check(running_lease.or(no_lease))
                    .check(terminal_finished.or(nonterminal_unfinished))
                    .check(
                        queued_attempt
                            .or(running_attempt)
                            .or(retryable_attempt)
                            .or(completed_attempt)
                            .or(cancelled_attempt),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_document_job_idempotency")
                    .table(DocumentJob::Table)
                    .col(DocumentJob::DocumentId)
                    .col(DocumentJob::RevisionToken)
                    .col(DocumentJob::Kind)
                    .col(DocumentJob::PipelineFingerprint)
                    .unique()
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_document_job_one_active")
                    .table(DocumentJob::Table)
                    .col(DocumentJob::DocumentId)
                    .unique()
                    .and_where(Expr::col(DocumentJob::Status).is_in([
                        DocumentJobStatus::Queued.as_str(),
                        DocumentJobStatus::Running.as_str(),
                        DocumentJobStatus::RetryWait.as_str(),
                    ]))
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_document_job_due")
                    .table(DocumentJob::Table)
                    .col(DocumentJob::Status)
                    .col(DocumentJob::AvailableAt)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_document_job_stale_lease")
                    .table(DocumentJob::Table)
                    .col(DocumentJob::Status)
                    .col(DocumentJob::LeaseExpiresAt)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_document_job_history")
                    .table(DocumentJob::Table)
                    .col(DocumentJob::DocumentId)
                    .col(DocumentJob::CreatedAt)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(DocumentJob::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(BlobRetirement::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Document::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(DocumentGeneration::Table).to_owned())
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum Project {
    Table,
    Id,
    Title,
    WorkspaceDir,
    CreatedAt,
}

#[derive(DeriveIden)]
enum Document {
    Table,
    Id,
    ProjectId,
    SourceUri,
    MediaType,
    Title,
    SourceBlobId,
    SourceSha256,
    SourceByteLen,
    CanonicalText,
    CanonicalFingerprint,
    SourceRegions,
    ContentRevision,
    RevisionToken,
    ProcessingStatus,
    IndexedRevision,
    IndexFingerprint,
    CreatedAt,
    UpdatedAt,
    IndexedAt,
}

#[derive(DeriveIden)]
enum DocumentGeneration {
    Table,
    DocumentId,
    ContentRevision,
    RevisionToken,
    Tombstone,
    RetirementPending,
    RetirementContentRevision,
    RetirementRevisionToken,
}

#[derive(DeriveIden)]
enum BlobRetirement {
    Table,
    BlobId,
    Status,
    AttemptCount,
    MaxAttempts,
    AvailableAt,
    LeaseToken,
    LeaseExpiresAt,
    StartedAt,
    FinishedAt,
    LastErrorCode,
    LastErrorDetail,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum DocumentJob {
    Table,
    Id,
    DocumentId,
    ContentRevision,
    RevisionToken,
    Kind,
    Status,
    PipelineFingerprint,
    AttemptCount,
    MaxAttempts,
    AvailableAt,
    LeaseToken,
    LeaseExpiresAt,
    StartedAt,
    FinishedAt,
    LastErrorCode,
    LastErrorDetail,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum Chat {
    Table,
    Id,
    ProjectId,
    Title,
    Model,
    WorkspaceDir,
    CreatedAt,
}

#[derive(DeriveIden)]
enum Message {
    Table,
    Id,
    ChatId,
    TurnId,
    Seq,
    Role,
    Content,
    CreatedAt,
}

#[derive(DeriveIden)]
enum MessageIdentity {
    Table,
    Id,
    ChatId,
    TurnId,
    Owner,
}

#[derive(DeriveIden)]
enum TurnRun {
    Table,
    Id,
    ChatId,
    InputMessageId,
    OutputMessageId,
    Model,
    Status,
    AttemptCount,
    MaxAttempts,
    AvailableAt,
    LeaseToken,
    LeaseExpiresAt,
    StartedAt,
    FinishedAt,
    LastErrorCode,
    LastErrorDetail,
    SteerRevision,
    LastSteerAppliedAt,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum TurnClaim {
    Table,
    Token,
    TurnId,
    AttemptCount,
    ClaimedAt,
    LeaseExpiresAt,
}

#[derive(DeriveIden)]
enum TurnClaimLock {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum TurnFailure {
    Table,
    LeaseToken,
    TurnId,
    AttemptCount,
    RequestedRetryAt,
    ErrorCode,
    ErrorDetail,
    ResolvedAt,
    ResultStatus,
}

#[derive(DeriveIden)]
enum TurnSteer {
    Table,
    Id,
    TurnId,
    ChatId,
    Content,
    Interrupt,
    Status,
    AppliedLeaseToken,
    MessageId,
    CreatedAt,
    ResolvedAt,
}

#[derive(DeriveIden)]
enum ToolCall {
    Table,
    Id,
    ChatId,
    TurnId,
    ProviderId,
    Name,
    Arguments,
    Result,
    IsError,
    CreatedAt,
    CompletedAt,
}

#[derive(DeriveIden)]
enum Setting {
    Table,
    Key,
    ValueJson,
}

#[derive(DeriveIden)]
enum Event {
    Table,
    ChatId,
    Seq,
    TurnId,
    LeaseToken,
    AttemptEventOrdinal,
    ScanToken,
    Terminal,
    Payload,
    CreatedAt,
}
