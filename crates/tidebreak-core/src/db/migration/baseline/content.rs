//! Document, output, attachment, and app tables of the schema baseline.

use sea_orm_migration::prelude::*;

use crate::db::migration::idens::*;

/// Authoritative imported documents: the canonical text of record plus the
/// content address of the raw bytes it was decoded from, when there were any.
///
/// `source_blob_id` is one of the schema's classes of live blob reference, and
/// the index on it exists because every retirement decision asks whether any
/// table still references the blob — that question must not scan.
pub(super) fn document_table() -> TableCreateStatement {
    Table::create()
        .table(Document::Table)
        .col(ColumnDef::new(Document::Id).uuid().not_null().primary_key())
        .col(ColumnDef::new(Document::ChatId).uuid())
        .col(ColumnDef::new(Document::ProjectId).uuid())
        .col(ColumnDef::new(Document::OriginUri).text())
        .col(ColumnDef::new(Document::MediaType).text().not_null())
        .col(ColumnDef::new(Document::Title).text())
        .col(ColumnDef::new(Document::SourceBlobId).uuid())
        .col(ColumnDef::new(Document::SourceSha256).binary())
        .col(ColumnDef::new(Document::SourceByteLen).big_integer())
        .col(ColumnDef::new(Document::CanonicalText).text().not_null())
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
        // The owner column holds the storage key of the principal the row
        // belongs to; owner-scoped store queries filter on it so one shared
        // database can partition cleanly by user. Unscoped (local-profile)
        // writers keep inserting the default.
        .col(
            ColumnDef::new(Document::Owner)
                .text()
                .not_null()
                .default("local"),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_document_project")
                .from(Document::Table, Document::ProjectId)
                .to(Project::Table, Project::Id)
                .on_delete(ForeignKeyAction::Restrict),
        )
        // A source is all-or-nothing: either the document is canonical text
        // only, or it carries a blob id, its digest, and its length together.
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
        .check(Expr::col(Document::MediaType).ne(""))
        .check(
            Expr::col(Document::OriginUri)
                .is_null()
                .or(Expr::col(Document::OriginUri).ne("")),
        )
        .to_owned()
}

pub(super) fn document_indexes() -> Vec<IndexCreateStatement> {
    vec![
        Index::create()
            .name("idx_document_project_created")
            .table(Document::Table)
            .col(Document::ProjectId)
            .col(Document::CreatedAt)
            .to_owned(),
        Index::create()
            .name("idx_document_source_blob")
            .table(Document::Table)
            .col(Document::SourceBlobId)
            .to_owned(),
        Index::create()
            .name("idx_document_chat_created")
            .table(Document::Table)
            .col(Document::ChatId)
            .col(Document::CreatedAt)
            .to_owned(),
    ]
}

/// Gives conversation outputs a durable record with an opaque identity and an
/// append-only revision history. The record — not the loose file in scratch —
/// is the authoritative catalog.
pub(super) fn output_table() -> TableCreateStatement {
    Table::create()
        .table(Output::Table)
        .col(ColumnDef::new(Output::Id).uuid().not_null().primary_key())
        .col(ColumnDef::new(Output::ChatId).uuid().not_null())
        .col(ColumnDef::new(Output::Filename).text().not_null())
        .col(ColumnDef::new(Output::MediaType).text().not_null())
        .col(ColumnDef::new(Output::CurrentRevisionId).uuid().not_null())
        .col(ColumnDef::new(Output::RevisionCount).integer().not_null())
        .col(
            ColumnDef::new(Output::CreatedAt)
                .timestamp_with_time_zone()
                .not_null(),
        )
        .col(
            ColumnDef::new(Output::UpdatedAt)
                .timestamp_with_time_zone()
                .not_null(),
        )
        .col(ColumnDef::new(Output::DeletedAt).timestamp_with_time_zone())
        .foreign_key(
            ForeignKey::create()
                .name("fk_output_chat")
                .from_tbl(Output::Table)
                .from_col(Output::ChatId)
                .to_tbl(Chat::Table)
                .to_col(Chat::Id)
                .on_delete(ForeignKeyAction::Cascade),
        )
        .check(
            Expr::col(Output::RevisionCount)
                .between(1, crate::deliverable::MAX_OUTPUT_REVISIONS as i32),
        )
        .to_owned()
}

pub(super) fn output_indexes() -> Vec<IndexCreateStatement> {
    vec![
        Index::create()
            .name("idx_output_chat_created")
            .table(Output::Table)
            .col(Output::ChatId)
            .col(Output::CreatedAt)
            .col(Output::Id)
            .to_owned(),
        // Filename is the identity everything outside the store addresses an
        // output by, so at most one live output in a conversation may carry a
        // given name. Retracted outputs are excluded: deleting `report.md` must
        // leave the name free for a later one.
        Index::create()
            .name("idx_output_chat_live_filename")
            .table(Output::Table)
            .col(Output::ChatId)
            .col(Output::Filename)
            .unique()
            .and_where(Expr::col(Output::DeletedAt).is_null())
            .to_owned(),
    ]
}

/// One immutable revision of an output: the length and digest of the bytes,
/// never the bytes themselves.
///
/// The `byte_len` upper bound is the binary cap — the outer limit that admits a
/// binary artifact at all. The tighter per-kind ceiling (text stays well below
/// it) is enforced in application validation.
pub(super) fn output_revision_table() -> TableCreateStatement {
    Table::create()
        .table(OutputRevision::Table)
        .col(
            ColumnDef::new(OutputRevision::Id)
                .uuid()
                .not_null()
                .primary_key(),
        )
        .col(ColumnDef::new(OutputRevision::OutputId).uuid().not_null())
        .col(ColumnDef::new(OutputRevision::Ordinal).integer().not_null())
        .col(
            ColumnDef::new(OutputRevision::ByteLen)
                .big_integer()
                .not_null(),
        )
        .col(
            ColumnDef::new(OutputRevision::Sha256)
                .binary_len(32)
                .not_null(),
        )
        .col(ColumnDef::new(OutputRevision::TurnId).uuid())
        .col(ColumnDef::new(OutputRevision::ProducingRunId).uuid())
        .col(
            ColumnDef::new(OutputRevision::CreatedAt)
                .timestamp_with_time_zone()
                .not_null(),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_output_revision_output")
                .from(OutputRevision::Table, OutputRevision::OutputId)
                .to(Output::Table, Output::Id)
                .on_delete(ForeignKeyAction::Cascade),
        )
        .check(
            Expr::col(OutputRevision::Ordinal)
                .between(1, crate::deliverable::MAX_OUTPUT_REVISIONS as i32),
        )
        .check(Expr::col(OutputRevision::ByteLen).gte(0))
        .check(
            Expr::col(OutputRevision::ByteLen)
                .lte(crate::deliverable::MAX_BINARY_DELIVERABLE_BYTES as i64),
        )
        // A revision names at most one producer: the foreground turn or the
        // background run.
        .check(
            Expr::col(OutputRevision::TurnId)
                .is_null()
                .or(Expr::col(OutputRevision::ProducingRunId).is_null()),
        )
        .to_owned()
}

pub(super) fn output_revision_indexes() -> Vec<IndexCreateStatement> {
    vec![Index::create()
        .name("idx_output_revision_ordinal")
        .table(OutputRevision::Table)
        .col(OutputRevision::OutputId)
        .col(OutputRevision::Ordinal)
        .unique()
        .to_owned()]
}

/// An assistant message's citations: an ordered list of direct document
/// locators, one row per citation.
pub(super) fn assistant_citation_table() -> TableCreateStatement {
    Table::create()
        .table(AssistantCitation::Table)
        .col(
            ColumnDef::new(AssistantCitation::Id)
                .uuid()
                .not_null()
                .primary_key(),
        )
        .col(
            ColumnDef::new(AssistantCitation::MessageId)
                .uuid()
                .not_null(),
        )
        .col(
            ColumnDef::new(AssistantCitation::Ordinal)
                .integer()
                .not_null(),
        )
        .col(
            ColumnDef::new(AssistantCitation::DocumentId)
                .uuid()
                .not_null(),
        )
        .col(
            ColumnDef::new(AssistantCitation::Locator)
                .json_binary()
                .not_null(),
        )
        .index(
            Index::create()
                .name("idx_assistant_citation_light_message_ordinal")
                .col(AssistantCitation::MessageId)
                .col(AssistantCitation::Ordinal)
                .unique(),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_assistant_citation_light_message")
                .from(AssistantCitation::Table, AssistantCitation::MessageId)
                .to(Message::Table, Message::Id)
                .on_delete(ForeignKeyAction::Cascade),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_assistant_citation_light_document")
                .from(AssistantCitation::Table, AssistantCitation::DocumentId)
                .to(Document::Table, Document::Id)
                .on_delete(ForeignKeyAction::Cascade),
        )
        .to_owned()
}

pub(super) fn assistant_citation_indexes() -> Vec<IndexCreateStatement> {
    Vec::new()
}

/// Reserves one validated content-addressed image for one chat.
///
/// Publication is authority, not merely a blob upload: knowing another chat's
/// content id must not let a caller bind those bytes into this chat. The
/// composite primary key makes exact retries idempotent while still requiring
/// identical bytes to be published separately to every chat that may attach
/// them. Metadata is retained so resolution can verify the blob still matches
/// the validated publication record.
///
/// This is a live blob reference. A published image remains attachable until
/// its chat is deleted, even if no message has used it yet.
pub(super) fn chat_image_publication_table() -> TableCreateStatement {
    Table::create()
        .table(ChatImagePublication::Table)
        .col(
            ColumnDef::new(ChatImagePublication::ChatId)
                .uuid()
                .not_null(),
        )
        .col(
            ColumnDef::new(ChatImagePublication::BlobId)
                .uuid()
                .not_null(),
        )
        .col(
            ColumnDef::new(ChatImagePublication::MediaType)
                .string_len(64)
                .not_null(),
        )
        .col(
            ColumnDef::new(ChatImagePublication::Width)
                .integer()
                .not_null(),
        )
        .col(
            ColumnDef::new(ChatImagePublication::Height)
                .integer()
                .not_null(),
        )
        .col(
            ColumnDef::new(ChatImagePublication::ByteLen)
                .big_integer()
                .not_null(),
        )
        .col(
            ColumnDef::new(ChatImagePublication::CreatedAt)
                .timestamp_with_time_zone()
                .not_null(),
        )
        .primary_key(
            Index::create()
                .col(ChatImagePublication::ChatId)
                .col(ChatImagePublication::BlobId),
        )
        // Restrict rather than cascade: chat deletion must first collect the
        // freed blob ids and enqueue their shared content for retirement.
        .foreign_key(
            ForeignKey::create()
                .name("fk_chat_image_publication_chat")
                .from(ChatImagePublication::Table, ChatImagePublication::ChatId)
                .to(Chat::Table, Chat::Id)
                .on_delete(ForeignKeyAction::Restrict),
        )
        .check(Expr::col(ChatImagePublication::BlobId).ne(uuid::Uuid::nil()))
        .check(Expr::col(ChatImagePublication::MediaType).is_in([
            crate::image::ImageMediaType::Png.as_str(),
            crate::image::ImageMediaType::Jpeg.as_str(),
            crate::image::ImageMediaType::Webp.as_str(),
            crate::image::ImageMediaType::Gif.as_str(),
        ]))
        .check(
            Expr::col(ChatImagePublication::Width)
                .between(1, crate::image::MAX_IMAGE_DIMENSION as i32),
        )
        .check(
            Expr::col(ChatImagePublication::Height)
                .between(1, crate::image::MAX_IMAGE_DIMENSION as i32),
        )
        .check(
            Expr::col(ChatImagePublication::ByteLen)
                .between(1, crate::image::MAX_IMAGE_BYTES as i64),
        )
        .to_owned()
}

pub(super) fn chat_image_publication_indexes() -> Vec<IndexCreateStatement> {
    vec![
        // Every retirement decision asks whether any chat still reserves the
        // shared blob; that lookup must not scan publications by chat.
        Index::create()
            .name("idx_chat_image_publication_blob")
            .table(ChatImagePublication::Table)
            .col(ChatImagePublication::BlobId)
            .to_owned(),
    ]
}

/// Records the images a message was submitted with, so a reloaded conversation
/// replays the same turn rather than a text-only approximation of it.
///
/// Only identity is stored: a content-addressed blob id plus the bounded
/// metadata a renderer or provider adapter needs. Filesystem paths are
/// deliberately absent — the bytes live in the blob store and are reachable
/// only through the blob id.
///
/// This makes `message_attachment.blob_id` another class of live blob
/// reference alongside document sources and chat publications. Blob liveness
/// is a union across all of them, computed in one place; see
/// `db::ops::blob::is_referenced_on`.
pub(super) fn message_attachment_table() -> TableCreateStatement {
    Table::create()
        .table(MessageAttachment::Table)
        .col(
            ColumnDef::new(MessageAttachment::MessageId)
                .uuid()
                .not_null(),
        )
        .col(
            ColumnDef::new(MessageAttachment::Ordinal)
                .integer()
                .not_null(),
        )
        .col(ColumnDef::new(MessageAttachment::ChatId).uuid().not_null())
        .col(ColumnDef::new(MessageAttachment::BlobId).uuid().not_null())
        .col(
            ColumnDef::new(MessageAttachment::MediaType)
                .string_len(64)
                .not_null(),
        )
        .col(
            ColumnDef::new(MessageAttachment::Width)
                .integer()
                .not_null(),
        )
        .col(
            ColumnDef::new(MessageAttachment::Height)
                .integer()
                .not_null(),
        )
        .col(
            ColumnDef::new(MessageAttachment::ByteLen)
                .big_integer()
                .not_null(),
        )
        .col(
            ColumnDef::new(MessageAttachment::CreatedAt)
                .timestamp_with_time_zone()
                .not_null(),
        )
        // The composite key is the ordering contract: one image per
        // position per message, so a retried submit cannot leave a
        // message holding two images at the same index.
        .primary_key(
            Index::create()
                .col(MessageAttachment::MessageId)
                .col(MessageAttachment::Ordinal),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_message_attachment_message")
                .from(MessageAttachment::Table, MessageAttachment::MessageId)
                .to(Message::Table, Message::Id)
                .on_delete(ForeignKeyAction::Restrict),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_message_attachment_chat")
                .from(MessageAttachment::Table, MessageAttachment::ChatId)
                .to(Chat::Table, Chat::Id)
                .on_delete(ForeignKeyAction::Restrict),
        )
        .check(Expr::col(MessageAttachment::BlobId).ne(uuid::Uuid::nil()))
        .check(Expr::col(MessageAttachment::Ordinal).gte(0))
        .check(
            Expr::col(MessageAttachment::Ordinal).lt(crate::model::MAX_MESSAGE_ATTACHMENTS as i32),
        )
        .check(Expr::col(MessageAttachment::MediaType).is_in([
            crate::image::ImageMediaType::Png.as_str(),
            crate::image::ImageMediaType::Jpeg.as_str(),
            crate::image::ImageMediaType::Webp.as_str(),
            crate::image::ImageMediaType::Gif.as_str(),
        ]))
        .check(
            Expr::col(MessageAttachment::Width)
                .between(1, crate::image::MAX_IMAGE_DIMENSION as i32),
        )
        .check(
            Expr::col(MessageAttachment::Height)
                .between(1, crate::image::MAX_IMAGE_DIMENSION as i32),
        )
        .check(
            Expr::col(MessageAttachment::ByteLen).between(1, crate::image::MAX_IMAGE_BYTES as i64),
        )
        .to_owned()
}

pub(super) fn message_attachment_indexes() -> Vec<IndexCreateStatement> {
    vec![
        // The orphan auditor and every retirement decision ask "does any
        // attachment still reference this blob?"; that lookup must not scan.
        Index::create()
            .name("idx_message_attachment_blob")
            .table(MessageAttachment::Table)
            .col(MessageAttachment::BlobId)
            .to_owned(),
        Index::create()
            .name("idx_message_attachment_chat")
            .table(MessageAttachment::Table)
            .col(MessageAttachment::ChatId)
            .col(MessageAttachment::MessageId)
            .col(MessageAttachment::Ordinal)
            .to_owned(),
    ]
}

/// Links imported source documents to the user message that introduced them.
pub(super) fn message_document_attachment_table() -> TableCreateStatement {
    Table::create()
        .table(MessageDocumentAttachment::Table)
        .col(
            ColumnDef::new(MessageDocumentAttachment::MessageId)
                .uuid()
                .not_null(),
        )
        .col(
            ColumnDef::new(MessageDocumentAttachment::Ordinal)
                .integer()
                .not_null(),
        )
        .col(
            ColumnDef::new(MessageDocumentAttachment::ChatId)
                .uuid()
                .not_null(),
        )
        .col(
            ColumnDef::new(MessageDocumentAttachment::DocumentId)
                .uuid()
                .not_null(),
        )
        .col(
            ColumnDef::new(MessageDocumentAttachment::CreatedAt)
                .timestamp_with_time_zone()
                .not_null(),
        )
        .primary_key(
            Index::create()
                .col(MessageDocumentAttachment::MessageId)
                .col(MessageDocumentAttachment::Ordinal),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_message_document_attachment_message")
                .from(
                    MessageDocumentAttachment::Table,
                    MessageDocumentAttachment::MessageId,
                )
                .to(Message::Table, Message::Id)
                .on_delete(ForeignKeyAction::Cascade),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_message_document_attachment_document")
                .from(
                    MessageDocumentAttachment::Table,
                    MessageDocumentAttachment::DocumentId,
                )
                .to(Document::Table, Document::Id)
                .on_delete(ForeignKeyAction::Cascade),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_message_document_attachment_chat")
                .from(
                    MessageDocumentAttachment::Table,
                    MessageDocumentAttachment::ChatId,
                )
                .to(Chat::Table, Chat::Id)
                .on_delete(ForeignKeyAction::Cascade),
        )
        .check(Expr::col(MessageDocumentAttachment::DocumentId).ne(uuid::Uuid::nil()))
        .check(Expr::col(MessageDocumentAttachment::Ordinal).gte(0))
        .check(
            Expr::col(MessageDocumentAttachment::Ordinal)
                .lt(crate::model::MAX_MESSAGE_ATTACHMENTS as i32),
        )
        .to_owned()
}

pub(super) fn message_document_attachment_indexes() -> Vec<IndexCreateStatement> {
    vec![
        Index::create()
            .name("idx_message_document_attachment_chat")
            .table(MessageDocumentAttachment::Table)
            .col(MessageDocumentAttachment::ChatId)
            .col(MessageDocumentAttachment::MessageId)
            .col(MessageDocumentAttachment::Ordinal)
            .to_owned(),
        Index::create()
            .name("idx_message_document_attachment_document")
            .table(MessageDocumentAttachment::Table)
            .col(MessageDocumentAttachment::DocumentId)
            .to_owned(),
        Index::create()
            .name("idx_message_document_attachment_unique")
            .table(MessageDocumentAttachment::Table)
            .col(MessageDocumentAttachment::MessageId)
            .col(MessageDocumentAttachment::DocumentId)
            .unique()
            .to_owned(),
    ]
}

/// The principal-owned local-app record: one row per app.
///
/// Follows `output`/`output_revision` with one deliberate difference: there is
/// no chat foreign key anywhere. The immutable owner on the app row keeps its
/// history alive after the authoring conversation is deleted while still
/// partitioning a shared database by principal.
pub(super) fn app_table() -> TableCreateStatement {
    Table::create()
        .table(App::Table)
        .col(ColumnDef::new(App::Id).uuid().not_null().primary_key())
        .col(
            ColumnDef::new(App::Owner)
                .text()
                .not_null()
                .default("local"),
        )
        .col(ColumnDef::new(App::Name).text().not_null())
        .col(ColumnDef::new(App::CurrentRevisionId).uuid().not_null())
        .col(ColumnDef::new(App::RevisionCount).integer().not_null())
        .col(
            ColumnDef::new(App::CreatedAt)
                .timestamp_with_time_zone()
                .not_null(),
        )
        .col(
            ColumnDef::new(App::UpdatedAt)
                .timestamp_with_time_zone()
                .not_null(),
        )
        .col(ColumnDef::new(App::DeletedAt).timestamp_with_time_zone())
        .check(Expr::col(App::RevisionCount).between(1, crate::local_app::MAX_APP_REVISIONS as i32))
        .to_owned()
}

pub(super) fn app_indexes() -> Vec<IndexCreateStatement> {
    vec![
        Index::create()
            .name("idx_app_updated")
            .table(App::Table)
            .col(App::UpdatedAt)
            .col(App::Id)
            .to_owned(),
        Index::create()
            .name("idx_app_owner_updated")
            .table(App::Table)
            .col(App::Owner)
            .col(App::UpdatedAt)
            .col(App::Id)
            .to_owned(),
    ]
}

/// Insert-only app revisions, each pairing a bounded manifest with the length
/// and digest of write-once bundle bytes.
pub(super) fn app_revision_table() -> TableCreateStatement {
    Table::create()
        .table(AppRevision::Table)
        .col(
            ColumnDef::new(AppRevision::Id)
                .uuid()
                .not_null()
                .primary_key(),
        )
        .col(ColumnDef::new(AppRevision::AppId).uuid().not_null())
        .col(ColumnDef::new(AppRevision::Ordinal).integer().not_null())
        .col(
            ColumnDef::new(AppRevision::ManifestJson)
                .json_binary()
                .not_null(),
        )
        .col(
            ColumnDef::new(AppRevision::ByteLen)
                .big_integer()
                .not_null(),
        )
        .col(
            ColumnDef::new(AppRevision::Sha256)
                .binary_len(32)
                .not_null(),
        )
        .col(ColumnDef::new(AppRevision::TurnId).uuid())
        .col(ColumnDef::new(AppRevision::ProducingRunId).uuid())
        // Provenance only: no foreign key, so the revision outlives
        // the conversation that authored it.
        .col(ColumnDef::new(AppRevision::ChatId).uuid())
        .col(
            ColumnDef::new(AppRevision::CreatedAt)
                .timestamp_with_time_zone()
                .not_null(),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_app_revision_app")
                .from_tbl(AppRevision::Table)
                .from_col(AppRevision::AppId)
                .to_tbl(App::Table)
                .to_col(App::Id)
                .on_delete(ForeignKeyAction::Cascade),
        )
        .check(
            Expr::col(AppRevision::Ordinal).between(1, crate::local_app::MAX_APP_REVISIONS as i32),
        )
        .check(
            Expr::col(AppRevision::ByteLen)
                .between(1, crate::local_app::MAX_APP_BUNDLE_BYTES as i64),
        )
        // A revision records the foreground turn or the background
        // run that produced it, never both.
        .check(
            Expr::col(AppRevision::TurnId)
                .is_null()
                .or(Expr::col(AppRevision::ProducingRunId).is_null()),
        )
        .to_owned()
}

pub(super) fn app_revision_indexes() -> Vec<IndexCreateStatement> {
    vec![Index::create()
        .name("idx_app_revision_ordinal")
        .table(AppRevision::Table)
        .col(AppRevision::AppId)
        .col(AppRevision::Ordinal)
        .unique()
        .to_owned()]
}

/// The durable app-grant consent record: at most one row per app — the app id
/// is the primary key — carrying the granted bindings as JSON.
///
/// The grant is host-computed policy, replaced wholesale by a fresh consent
/// and deleted by revocation, so the table needs no history and no surrogate
/// identity. Cascade delete follows the app row: a grant never outlives the
/// thing it consented to.
pub(super) fn app_grant_table() -> TableCreateStatement {
    Table::create()
        .table(AppGrant::Table)
        .col(
            ColumnDef::new(AppGrant::AppId)
                .uuid()
                .not_null()
                .primary_key(),
        )
        .col(
            ColumnDef::new(AppGrant::BindingsJson)
                .json_binary()
                .not_null(),
        )
        .col(
            ColumnDef::new(AppGrant::CreatedAt)
                .timestamp_with_time_zone()
                .not_null(),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_app_grant_app")
                .from_tbl(AppGrant::Table)
                .from_col(AppGrant::AppId)
                .to_tbl(App::Table)
                .to_col(App::Id)
                .on_delete(ForeignKeyAction::Cascade),
        )
        .to_owned()
}

pub(super) fn app_grant_indexes() -> Vec<IndexCreateStatement> {
    Vec::new()
}

/// The gateway-side registration one local app holds at one deployment: the
/// shared app it was registered as, the gateway revision that registration is
/// currently serving, and the local revision that revision was projected from.
///
/// The key is `(app_id, gateway_base_url)` because a registration belongs to
/// one deployment. A profile re-paired to a different gateway finds no row
/// there and registers afresh; the rows the previous pairing left are orphaned
/// rather than misread, and nothing sweeps them — they die with the app row.
pub(super) fn app_gateway_draft_table() -> TableCreateStatement {
    Table::create()
        .table(AppGatewayDraft::Table)
        .col(ColumnDef::new(AppGatewayDraft::AppId).uuid().not_null())
        .col(
            ColumnDef::new(AppGatewayDraft::GatewayBaseUrl)
                .text()
                .not_null(),
        )
        .col(
            ColumnDef::new(AppGatewayDraft::SharedAppId)
                .text()
                .not_null(),
        )
        .col(
            ColumnDef::new(AppGatewayDraft::GatewayRevisionId)
                .text()
                .not_null(),
        )
        .col(
            ColumnDef::new(AppGatewayDraft::SyncedRevisionId)
                .uuid()
                .not_null(),
        )
        .col(
            ColumnDef::new(AppGatewayDraft::UpdatedAt)
                .timestamp_with_time_zone()
                .not_null(),
        )
        .primary_key(
            Index::create()
                .col(AppGatewayDraft::AppId)
                .col(AppGatewayDraft::GatewayBaseUrl),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_app_gateway_draft_app")
                .from(AppGatewayDraft::Table, AppGatewayDraft::AppId)
                .to(App::Table, App::Id)
                .on_delete(ForeignKeyAction::Cascade),
        )
        .check(Expr::col(AppGatewayDraft::GatewayBaseUrl).ne(""))
        .check(Expr::col(AppGatewayDraft::SharedAppId).ne(""))
        .check(Expr::col(AppGatewayDraft::GatewayRevisionId).ne(""))
        .to_owned()
}

pub(super) fn app_gateway_draft_indexes() -> Vec<IndexCreateStatement> {
    Vec::new()
}

/// The profile-scoped connected-app record (docs/connected-apps.md), including
/// the persisted MCP server configuration: one `mcp_server`-kind row per
/// server.
pub(super) fn connected_app_table() -> TableCreateStatement {
    Table::create()
        .table(ConnectedApp::Table)
        .col(
            ColumnDef::new(ConnectedApp::Id)
                .uuid()
                .not_null()
                .primary_key(),
        )
        .col(ColumnDef::new(ConnectedApp::Name).string().not_null())
        .col(ColumnDef::new(ConnectedApp::Kind).string().not_null())
        .col(
            ColumnDef::new(ConnectedApp::DefinitionJson)
                .json_binary()
                .not_null(),
        )
        .col(ColumnDef::new(ConnectedApp::Position).integer().not_null())
        .col(
            ColumnDef::new(ConnectedApp::CreatedAt)
                .timestamp_with_time_zone()
                .not_null(),
        )
        .col(
            ColumnDef::new(ConnectedApp::UpdatedAt)
                .timestamp_with_time_zone()
                .not_null(),
        )
        .to_owned()
}

pub(super) fn connected_app_indexes() -> Vec<IndexCreateStatement> {
    vec![
        // For `mcp_server` rows the name is the mount namespace; two records
        // may not claim one namespace. Scoped by kind so a REST entry may
        // share a display name with a server.
        Index::create()
            .name("idx_connected_app_kind_name")
            .table(ConnectedApp::Table)
            .col(ConnectedApp::Kind)
            .col(ConnectedApp::Name)
            .unique()
            .to_owned(),
    ]
}
