use sea_orm_migration::prelude::*;

use super::{BlobRetirementStatus, DocumentJobKind, DocumentJobStatus, DocumentProcessingStatus};

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
                    .table(Message::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Message::Id).uuid().not_null().primary_key())
                    .col(ColumnDef::new(Message::ChatId).uuid().not_null())
                    .col(ColumnDef::new(Message::TurnId).uuid().not_null())
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
                    .col(Message::CreatedAt)
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

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Message::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Setting::Table).to_owned())
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
    Role,
    Content,
    CreatedAt,
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
    Payload,
    CreatedAt,
}
