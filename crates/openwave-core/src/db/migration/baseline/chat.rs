//! Chat, message, and journal tables of the schema baseline.

use sea_orm_migration::prelude::*;

use crate::db::migration::idens::*;

pub(super) fn project_table() -> TableCreateStatement {
    Table::create()
        .table(Project::Table)
        .col(ColumnDef::new(Project::Id).uuid().not_null().primary_key())
        .col(ColumnDef::new(Project::Title).text())
        .col(
            ColumnDef::new(Project::AttachmentRevision)
                .big_integer()
                .not_null()
                .default(0),
        )
        .col(
            ColumnDef::new(Project::CreatedAt)
                .timestamp_with_time_zone()
                .not_null(),
        )
        .col(
            ColumnDef::new(Project::Owner)
                .text()
                .not_null()
                .default("local"),
        )
        .check(Expr::col(Project::AttachmentRevision).gte(0))
        .check(Expr::col(Project::AttachmentRevision).lte(crate::model::MAX_ATTACHMENT_REVISION))
        .to_owned()
}

pub(super) fn project_indexes() -> Vec<IndexCreateStatement> {
    vec![]
}

pub(super) fn project_root_attachment_table() -> TableCreateStatement {
    Table::create()
        .table(ProjectRootAttachment::Table)
        .col(
            ColumnDef::new(ProjectRootAttachment::ProjectId)
                .uuid()
                .not_null(),
        )
        .col(
            ColumnDef::new(ProjectRootAttachment::RootId)
                .uuid()
                .not_null(),
        )
        .col(
            ColumnDef::new(ProjectRootAttachment::Position)
                .integer()
                .not_null(),
        )
        .primary_key(
            Index::create()
                .col(ProjectRootAttachment::ProjectId)
                .col(ProjectRootAttachment::RootId),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_project_root_attachment_project")
                .from(
                    ProjectRootAttachment::Table,
                    ProjectRootAttachment::ProjectId,
                )
                .to(Project::Table, Project::Id)
                .on_delete(ForeignKeyAction::Cascade),
        )
        .check(Expr::col(ProjectRootAttachment::RootId).ne(uuid::Uuid::nil()))
        .check(Expr::col(ProjectRootAttachment::Position).gte(0))
        .check(
            Expr::col(ProjectRootAttachment::Position)
                .lt(crate::model::MAX_ROOT_ATTACHMENTS as i32),
        )
        .to_owned()
}

pub(super) fn project_root_attachment_indexes() -> Vec<IndexCreateStatement> {
    vec![Index::create()
        .name("idx_project_root_attachment_position")
        .table(ProjectRootAttachment::Table)
        .col(ProjectRootAttachment::ProjectId)
        .col(ProjectRootAttachment::Position)
        .unique()
        .to_owned()]
}

pub(super) fn chat_table() -> TableCreateStatement {
    Table::create()
        .table(Chat::Table)
        .col(ColumnDef::new(Chat::Id).uuid().not_null().primary_key())
        .col(ColumnDef::new(Chat::ProjectId).uuid())
        .col(ColumnDef::new(Chat::Title).text())
        .col(
            ColumnDef::new(Chat::AttachmentRevision)
                .big_integer()
                .not_null()
                .default(0),
        )
        .col(
            ColumnDef::new(Chat::CreatedAt)
                .timestamp_with_time_zone()
                .not_null(),
        )
        .col(ColumnDef::new(Chat::Model).text())
        .col(ColumnDef::new(Chat::ReasoningEffort).text())
        // `NULL` reads as `ask`.
        .col(ColumnDef::new(Chat::PermissionMode).text())
        // Provider-neutral per-chat code-execution network policy; the native
        // sandbox's historical behavior is off.
        .col(
            ColumnDef::new(Chat::NetworkPolicy)
                .text()
                .not_null()
                .default(r#"{"mode":"off"}"#),
        )
        .col(
            ColumnDef::new(Chat::Owner)
                .text()
                .not_null()
                .default("local"),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_chat_project")
                .from(Chat::Table, Chat::ProjectId)
                .to(Project::Table, Project::Id)
                .on_delete(ForeignKeyAction::Restrict),
        )
        .check(Expr::col(Chat::AttachmentRevision).gte(0))
        .check(Expr::col(Chat::AttachmentRevision).lte(crate::model::MAX_ATTACHMENT_REVISION))
        .to_owned()
}

pub(super) fn chat_indexes() -> Vec<IndexCreateStatement> {
    vec![]
}

pub(super) fn chat_root_attachment_table() -> TableCreateStatement {
    Table::create()
        .table(ChatRootAttachment::Table)
        .col(ColumnDef::new(ChatRootAttachment::ChatId).uuid().not_null())
        .col(ColumnDef::new(ChatRootAttachment::RootId).uuid().not_null())
        .col(
            ColumnDef::new(ChatRootAttachment::Position)
                .integer()
                .not_null(),
        )
        .col(
            ColumnDef::new(ChatRootAttachment::Origin)
                .string_len(24)
                .not_null(),
        )
        .primary_key(
            Index::create()
                .col(ChatRootAttachment::ChatId)
                .col(ChatRootAttachment::RootId),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_chat_root_attachment_chat")
                .from(ChatRootAttachment::Table, ChatRootAttachment::ChatId)
                .to(Chat::Table, Chat::Id)
                .on_delete(ForeignKeyAction::Cascade),
        )
        .check(Expr::col(ChatRootAttachment::RootId).ne(uuid::Uuid::nil()))
        .check(Expr::col(ChatRootAttachment::Position).gte(0))
        .check(
            Expr::col(ChatRootAttachment::Position).lt(crate::model::MAX_ROOT_ATTACHMENTS as i32),
        )
        .check(Expr::col(ChatRootAttachment::Origin).is_in(["project_default", "conversation"]))
        .to_owned()
}

pub(super) fn chat_root_attachment_indexes() -> Vec<IndexCreateStatement> {
    vec![Index::create()
        .name("idx_chat_root_attachment_position")
        .table(ChatRootAttachment::Table)
        .col(ChatRootAttachment::ChatId)
        .col(ChatRootAttachment::Position)
        .unique()
        .to_owned()]
}

pub(super) fn root_attachment_change_table() -> TableCreateStatement {
    let projection_metadata_present = Expr::col(RootAttachmentChange::ProjectionExistedBefore)
        .eq(true)
        .or(Expr::col(RootAttachmentChange::Action).eq("attach"));
    let projection_metadata_absent = Expr::col(RootAttachmentChange::ProjectionExistedBefore)
        .eq(false)
        .and(Expr::col(RootAttachmentChange::Action).eq("detach"));
    let awaiting_broker = Expr::col(RootAttachmentChange::Phase)
        .eq("awaiting_broker")
        .and(Expr::col(RootAttachmentChange::ResultRevision).is_null())
        .and(Expr::col(RootAttachmentChange::ProjectionChanged).is_null())
        .and(Expr::col(RootAttachmentChange::BrokerChanged).is_null())
        .and(Expr::col(RootAttachmentChange::BrokerCurrentlyAttached).is_null())
        .and(Expr::col(RootAttachmentChange::FailureCode).is_null())
        .and(Expr::col(RootAttachmentChange::FailureMessage).is_null())
        .and(Expr::col(RootAttachmentChange::FailureRetryable).is_null())
        .and(Expr::col(RootAttachmentChange::FinishedAt).is_null());
    let completed = Expr::col(RootAttachmentChange::Phase)
        .eq("completed")
        .and(Expr::col(RootAttachmentChange::ResultRevision).is_not_null())
        .and(Expr::col(RootAttachmentChange::ProjectionChanged).is_not_null())
        .and(Expr::col(RootAttachmentChange::BrokerChanged).is_not_null())
        .and(Expr::col(RootAttachmentChange::BrokerCurrentlyAttached).is_not_null())
        .and(Expr::col(RootAttachmentChange::FailureCode).is_null())
        .and(Expr::col(RootAttachmentChange::FailureMessage).is_null())
        .and(Expr::col(RootAttachmentChange::FailureRetryable).is_null())
        .and(Expr::col(RootAttachmentChange::FinishedAt).is_not_null());
    let failed = Expr::col(RootAttachmentChange::Phase)
        .eq("failed")
        .and(Expr::col(RootAttachmentChange::ResultRevision).is_not_null())
        .and(Expr::col(RootAttachmentChange::ProjectionChanged).is_not_null())
        .and(Expr::col(RootAttachmentChange::FailureCode).is_not_null())
        .and(Expr::col(RootAttachmentChange::FailureMessage).is_not_null())
        .and(Expr::col(RootAttachmentChange::FailureRetryable).is_not_null())
        .and(Expr::col(RootAttachmentChange::FinishedAt).is_not_null());

    Table::create()
        .table(RootAttachmentChange::Table)
        .col(
            ColumnDef::new(RootAttachmentChange::Id)
                .uuid()
                .not_null()
                .primary_key(),
        )
        .col(
            ColumnDef::new(RootAttachmentChange::ChatId)
                .uuid()
                .not_null(),
        )
        .col(
            ColumnDef::new(RootAttachmentChange::SubjectKind)
                .string_len(24)
                .not_null(),
        )
        .col(
            ColumnDef::new(RootAttachmentChange::SubjectId)
                .uuid()
                .not_null(),
        )
        .col(
            ColumnDef::new(RootAttachmentChange::ExecutorId)
                .uuid()
                .not_null(),
        )
        .col(
            ColumnDef::new(RootAttachmentChange::RootId)
                .uuid()
                .not_null(),
        )
        .col(
            ColumnDef::new(RootAttachmentChange::Action)
                .string_len(16)
                .not_null(),
        )
        .col(ColumnDef::new(RootAttachmentChange::Origin).string_len(24))
        .col(ColumnDef::new(RootAttachmentChange::ProjectionPosition).integer())
        .col(
            ColumnDef::new(RootAttachmentChange::ProjectionExistedBefore)
                .boolean()
                .not_null(),
        )
        .col(
            ColumnDef::new(RootAttachmentChange::ExpectedRevision)
                .big_integer()
                .not_null(),
        )
        .col(
            ColumnDef::new(RootAttachmentChange::BeforeRevision)
                .big_integer()
                .not_null(),
        )
        .col(
            ColumnDef::new(RootAttachmentChange::IntentRevision)
                .big_integer()
                .not_null(),
        )
        .col(
            ColumnDef::new(RootAttachmentChange::Phase)
                .string_len(24)
                .not_null(),
        )
        .col(ColumnDef::new(RootAttachmentChange::ResultRevision).big_integer())
        .col(ColumnDef::new(RootAttachmentChange::ProjectionChanged).boolean())
        .col(ColumnDef::new(RootAttachmentChange::BrokerChanged).boolean())
        .col(ColumnDef::new(RootAttachmentChange::BrokerCurrentlyAttached).boolean())
        .col(ColumnDef::new(RootAttachmentChange::FailureCode).string_len(64))
        .col(ColumnDef::new(RootAttachmentChange::FailureMessage).string_len(256))
        .col(ColumnDef::new(RootAttachmentChange::FailureRetryable).boolean())
        .col(
            ColumnDef::new(RootAttachmentChange::CreatedAt)
                .timestamp_with_time_zone()
                .not_null(),
        )
        .col(ColumnDef::new(RootAttachmentChange::FinishedAt).timestamp_with_time_zone())
        .foreign_key(
            ForeignKey::create()
                .name("fk_root_attachment_change_chat")
                .from(RootAttachmentChange::Table, RootAttachmentChange::ChatId)
                .to(Chat::Table, Chat::Id)
                .on_delete(ForeignKeyAction::Restrict),
        )
        .check(Expr::col(RootAttachmentChange::Id).ne(uuid::Uuid::nil()))
        .check(Expr::col(RootAttachmentChange::ChatId).ne(uuid::Uuid::nil()))
        .check(Expr::col(RootAttachmentChange::SubjectId).ne(uuid::Uuid::nil()))
        .check(Expr::col(RootAttachmentChange::ExecutorId).ne(uuid::Uuid::nil()))
        .check(Expr::col(RootAttachmentChange::RootId).ne(uuid::Uuid::nil()))
        .check(Expr::col(RootAttachmentChange::SubjectKind).is_in(["project", "conversation"]))
        .check(Expr::col(RootAttachmentChange::Action).is_in(["attach", "detach"]))
        .check(
            Expr::col(RootAttachmentChange::Origin)
                .is_null()
                .or(Expr::col(RootAttachmentChange::Origin)
                    .is_in(["project_default", "conversation"])),
        )
        .check(
            projection_metadata_present
                .and(Expr::col(RootAttachmentChange::Origin).is_not_null())
                .and(Expr::col(RootAttachmentChange::ProjectionPosition).is_not_null())
                .or(projection_metadata_absent
                    .and(Expr::col(RootAttachmentChange::Origin).is_null())
                    .and(Expr::col(RootAttachmentChange::ProjectionPosition).is_null())),
        )
        .check(
            Expr::col(RootAttachmentChange::ProjectionPosition)
                .is_null()
                .or(Expr::col(RootAttachmentChange::ProjectionPosition)
                    .between(0, crate::model::MAX_ROOT_ATTACHMENTS as i32 - 1)),
        )
        .check(
            Expr::col(RootAttachmentChange::Action)
                .ne("attach")
                .or(Expr::col(RootAttachmentChange::ProjectionExistedBefore).eq(true))
                .or(Expr::col(RootAttachmentChange::Origin).eq("conversation")),
        )
        .check(
            Expr::col(RootAttachmentChange::ExpectedRevision)
                .between(0, crate::model::MAX_ATTACHMENT_REVISION),
        )
        .check(
            Expr::col(RootAttachmentChange::BeforeRevision)
                .between(0, crate::model::MAX_ATTACHMENT_REVISION),
        )
        .check(
            Expr::col(RootAttachmentChange::IntentRevision)
                .between(0, crate::model::MAX_ATTACHMENT_REVISION),
        )
        .check(
            Expr::col(RootAttachmentChange::ResultRevision)
                .is_null()
                .or(Expr::col(RootAttachmentChange::ResultRevision)
                    .between(0, crate::model::MAX_ATTACHMENT_REVISION)),
        )
        .check(
            Expr::col(RootAttachmentChange::ExpectedRevision)
                .equals(RootAttachmentChange::BeforeRevision),
        )
        .check(
            Expr::col(RootAttachmentChange::IntentRevision)
                .equals(RootAttachmentChange::BeforeRevision)
                .or(Expr::col(RootAttachmentChange::IntentRevision)
                    .eq(Expr::col(RootAttachmentChange::BeforeRevision).add(1))),
        )
        .check(
            Expr::col(RootAttachmentChange::Action)
                .ne("attach")
                .or(Expr::col(RootAttachmentChange::ProjectionExistedBefore).eq(true))
                .or(Expr::col(RootAttachmentChange::IntentRevision)
                    .eq(Expr::col(RootAttachmentChange::BeforeRevision).add(1))),
        )
        .check(
            Expr::col(RootAttachmentChange::Action)
                .eq("attach")
                .and(Expr::col(RootAttachmentChange::ProjectionExistedBefore).eq(false))
                .or(Expr::col(RootAttachmentChange::IntentRevision)
                    .equals(RootAttachmentChange::BeforeRevision)),
        )
        .check(
            Expr::col(RootAttachmentChange::Action)
                .ne("attach")
                .or(Expr::col(RootAttachmentChange::ProjectionExistedBefore).eq(true))
                .or(Expr::col(RootAttachmentChange::BeforeRevision)
                    .lte(crate::model::MAX_ATTACHMENT_REVISION - 2)),
        )
        .check(
            Expr::col(RootAttachmentChange::Action)
                .ne("detach")
                .or(Expr::col(RootAttachmentChange::ProjectionExistedBefore).eq(false))
                .or(Expr::col(RootAttachmentChange::BeforeRevision)
                    .lte(crate::model::MAX_ATTACHMENT_REVISION - 1)),
        )
        .check(
            Expr::col(RootAttachmentChange::FailureCode)
                .is_null()
                .or(Func::char_length(Expr::col(RootAttachmentChange::FailureCode)).between(1, 64)),
        )
        .check(
            Expr::col(RootAttachmentChange::FailureMessage)
                .is_null()
                .or(
                    Func::char_length(Expr::col(RootAttachmentChange::FailureMessage))
                        .between(1, 256),
                ),
        )
        .check(awaiting_broker.or(completed).or(failed))
        .check(
            Expr::col(RootAttachmentChange::FinishedAt)
                .is_null()
                .or(Expr::col(RootAttachmentChange::FinishedAt)
                    .gte(Expr::col(RootAttachmentChange::CreatedAt))),
        )
        .to_owned()
}

pub(super) fn root_attachment_change_indexes() -> Vec<IndexCreateStatement> {
    vec![
        Index::create()
            .name("idx_root_attachment_change_one_awaiting")
            .table(RootAttachmentChange::Table)
            .col(RootAttachmentChange::ChatId)
            .unique()
            .and_where(Expr::col(RootAttachmentChange::Phase).eq("awaiting_broker"))
            .to_owned(),
        Index::create()
            .name("idx_root_attachment_change_pending_scan")
            .table(RootAttachmentChange::Table)
            .col(RootAttachmentChange::ExecutorId)
            .col(RootAttachmentChange::Phase)
            .col(RootAttachmentChange::CreatedAt)
            .col(RootAttachmentChange::Id)
            .to_owned(),
        Index::create()
            .name("idx_root_attachment_change_history")
            .table(RootAttachmentChange::Table)
            .col(RootAttachmentChange::ChatId)
            .col(RootAttachmentChange::CreatedAt)
            .col(RootAttachmentChange::Id)
            .to_owned(),
    ]
}

pub(super) fn setting_table() -> TableCreateStatement {
    Table::create()
        .table(Setting::Table)
        .col(ColumnDef::new(Setting::Key).text().not_null().primary_key())
        .col(ColumnDef::new(Setting::ValueJson).json_binary().not_null())
        .to_owned()
}

pub(super) fn setting_indexes() -> Vec<IndexCreateStatement> {
    vec![]
}

pub(super) fn message_identity_table() -> TableCreateStatement {
    Table::create()
        .table(MessageIdentity::Table)
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
        .to_owned()
}

pub(super) fn message_identity_indexes() -> Vec<IndexCreateStatement> {
    vec![]
}

pub(super) fn message_table() -> TableCreateStatement {
    Table::create()
        .table(Message::Table)
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
        .col(ColumnDef::new(Message::TurnLeaseToken).uuid())
        .foreign_key(
            ForeignKey::create()
                .name("fk_message_chat")
                .from(Message::Table, Message::ChatId)
                .to(Chat::Table, Chat::Id),
        )
        .to_owned()
}

pub(super) fn message_indexes() -> Vec<IndexCreateStatement> {
    vec![
        Index::create()
            .name("idx_message_chat")
            .table(Message::Table)
            .col(Message::ChatId)
            .col(Message::Seq)
            .unique()
            .to_owned(),
        Index::create()
            .name("idx_message_turn_identity")
            .table(Message::Table)
            .col(Message::Id)
            .col(Message::ChatId)
            .col(Message::TurnId)
            .unique()
            .to_owned(),
    ]
}

pub(super) fn blob_retirement_table() -> TableCreateStatement {
    use crate::model::BlobRetirementStatus;

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

    Table::create()
        .table(BlobRetirement::Table)
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
                    .between(1, crate::model::BlobRetirement::MAX_ERROR_CODE_LEN as i32)),
        )
        .check(
            Expr::col(BlobRetirement::LastErrorDetail)
                .is_null()
                .or(
                    Func::char_length(Expr::col(BlobRetirement::LastErrorDetail))
                        .between(1, crate::model::BlobRetirement::MAX_ERROR_DETAIL_LEN as i32),
                ),
        )
        .to_owned()
}

pub(super) fn blob_retirement_indexes() -> Vec<IndexCreateStatement> {
    vec![
        Index::create()
            .name("idx_blob_retirement_due")
            .table(BlobRetirement::Table)
            .col(BlobRetirement::Status)
            .col(BlobRetirement::AvailableAt)
            .to_owned(),
        Index::create()
            .name("idx_blob_retirement_stale_lease")
            .table(BlobRetirement::Table)
            .col(BlobRetirement::Status)
            .col(BlobRetirement::LeaseExpiresAt)
            .to_owned(),
    ]
}

pub(super) fn event_table() -> TableCreateStatement {
    Table::create()
        .table(Event::Table)
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
        .to_owned()
}

pub(super) fn event_indexes() -> Vec<IndexCreateStatement> {
    vec![
        Index::create()
            .name("idx_event_attempt_ordinal")
            .table(Event::Table)
            .col(Event::LeaseToken)
            .col(Event::AttemptEventOrdinal)
            .unique()
            .to_owned(),
        Index::create()
            .name("idx_event_scan_token")
            .table(Event::Table)
            .col(Event::ScanToken)
            .unique()
            .to_owned(),
        Index::create()
            .name("idx_event_one_terminal_per_turn")
            .table(Event::Table)
            .col(Event::TurnId)
            .unique()
            .and_where(Expr::col(Event::Terminal).eq(true))
            .to_owned(),
    ]
}
