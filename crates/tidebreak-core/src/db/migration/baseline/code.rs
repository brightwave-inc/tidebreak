//! Code-mode tables. No foreign keys into chat tables — the surfaces are
//! type-and-schema separated.
//!
//! Every table here carries an `owner` column holding the storage key of the
//! principal the row belongs to, the way chat, document, and app rows do. The
//! column is denormalized onto child rows rather than reached through a join,
//! so an owner-scoped query exists for every table and a shared database
//! partitions cleanly by user. On the desktop profile every row belongs to
//! the single local owner and keeps the default.
//!
//! Repositories are owned too: nothing in code mode is shared across owners,
//! so two users may register or clone the same remote independently.

use sea_orm_migration::prelude::*;

use crate::db::migration::idens::*;

/// Storage key written by unscoped (local-profile) writers.
const LOCAL_OWNER: &str = "local";

fn owner_column<T: IntoIden>(column: T) -> ColumnDef {
    ColumnDef::new(column)
        .text()
        .not_null()
        .default(LOCAL_OWNER)
        .to_owned()
}

pub(super) fn code_repo_table() -> TableCreateStatement {
    Table::create()
        .table(CodeRepo::Table)
        .col(ColumnDef::new(CodeRepo::Id).uuid().not_null().primary_key())
        .col(owner_column(CodeRepo::Owner))
        // Registration paths are unique per owner, not per install: two users
        // on a shared machine may each register the same path, and each one
        // clones into an owner-keyed directory of their own.
        .col(ColumnDef::new(CodeRepo::RootPath).text().not_null())
        .col(ColumnDef::new(CodeRepo::DisplayName).text().not_null())
        .col(ColumnDef::new(CodeRepo::DefaultBaseRef).text().not_null())
        .col(ColumnDef::new(CodeRepo::BranchPrefix).text().not_null())
        .col(ColumnDef::new(CodeRepo::SetupScript).text())
        .col(ColumnDef::new(CodeRepo::ArchiveScript).text())
        .col(
            ColumnDef::new(CodeRepo::QuickActions)
                .json_binary()
                .not_null(),
        )
        .col(
            ColumnDef::new(CodeRepo::CreatedAt)
                .timestamp_with_time_zone()
                .not_null(),
        )
        .to_owned()
}

pub(super) fn code_repo_indexes() -> Vec<IndexCreateStatement> {
    vec![Index::create()
        .name("idx_code_repo_owner_root_path")
        .table(CodeRepo::Table)
        .col(CodeRepo::Owner)
        .col(CodeRepo::RootPath)
        .unique()
        .to_owned()]
}

pub(super) fn code_workspace_table() -> TableCreateStatement {
    Table::create()
        .table(CodeWorkspace::Table)
        .col(
            ColumnDef::new(CodeWorkspace::Id)
                .uuid()
                .not_null()
                .primary_key(),
        )
        .col(owner_column(CodeWorkspace::Owner))
        .col(ColumnDef::new(CodeWorkspace::RepoId).uuid().not_null())
        .col(ColumnDef::new(CodeWorkspace::Title).text().not_null())
        .col(
            ColumnDef::new(CodeWorkspace::WorktreePath)
                .text()
                .not_null()
                .unique_key(),
        )
        .col(ColumnDef::new(CodeWorkspace::BranchName).text().not_null())
        .col(ColumnDef::new(CodeWorkspace::BaseRef).text().not_null())
        .col(ColumnDef::new(CodeWorkspace::Status).text().not_null())
        .col(ColumnDef::new(CodeWorkspace::Pr).json_binary())
        .col(
            ColumnDef::new(CodeWorkspace::CreatedAt)
                .timestamp_with_time_zone()
                .not_null(),
        )
        .col(ColumnDef::new(CodeWorkspace::ArchivedAt).timestamp_with_time_zone())
        .foreign_key(
            ForeignKey::create()
                .name("fk_code_workspace_repo")
                .from(CodeWorkspace::Table, CodeWorkspace::RepoId)
                .to(CodeRepo::Table, CodeRepo::Id),
        )
        .check(Expr::col(CodeWorkspace::Status).is_in([
            "creating",
            "setup_failed",
            "active",
            "archived",
        ]))
        .to_owned()
}

pub(super) fn code_workspace_indexes() -> Vec<IndexCreateStatement> {
    vec![
        Index::create()
            .name("idx_code_workspace_repo_branch")
            .table(CodeWorkspace::Table)
            .col(CodeWorkspace::RepoId)
            .col(CodeWorkspace::BranchName)
            .unique()
            .to_owned(),
        Index::create()
            .name("idx_code_workspace_owner_created")
            .table(CodeWorkspace::Table)
            .col(CodeWorkspace::Owner)
            .col(CodeWorkspace::CreatedAt)
            .to_owned(),
    ]
}

pub(super) fn code_session_table() -> TableCreateStatement {
    Table::create()
        .table(CodeSession::Table)
        .col(
            ColumnDef::new(CodeSession::Id)
                .uuid()
                .not_null()
                .primary_key(),
        )
        .col(owner_column(CodeSession::Owner))
        .col(ColumnDef::new(CodeSession::WorkspaceId).uuid().not_null())
        .col(
            ColumnDef::new(CodeSession::Kind)
                .text()
                .not_null()
                .default("interactive"),
        )
        .col(ColumnDef::new(CodeSession::HarnessKind).text().not_null())
        .col(ColumnDef::new(CodeSession::HarnessVersion).text())
        .col(ColumnDef::new(CodeSession::HarnessResumeRef).text())
        .col(
            ColumnDef::new(CodeSession::PermissionMode)
                .text()
                .not_null(),
        )
        .col(ColumnDef::new(CodeSession::Model).text())
        .col(ColumnDef::new(CodeSession::Lifecycle).text().not_null())
        .col(ColumnDef::new(CodeSession::FenceReason).json_binary())
        .col(ColumnDef::new(CodeSession::ChildPid).big_integer())
        .col(
            ColumnDef::new(CodeSession::SpawnEpoch)
                .big_integer()
                .not_null()
                .default(0),
        )
        .col(
            ColumnDef::new(CodeSession::AttentionState)
                .json_binary()
                .not_null(),
        )
        .col(
            ColumnDef::new(CodeSession::AttentionSource)
                .text()
                .not_null(),
        )
        .col(
            ColumnDef::new(CodeSession::UnrecognizedEventCount)
                .big_integer()
                .not_null()
                .default(0),
        )
        .col(
            ColumnDef::new(CodeSession::CreatedAt)
                .timestamp_with_time_zone()
                .not_null(),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_code_session_workspace")
                .from(CodeSession::Table, CodeSession::WorkspaceId)
                .to(CodeWorkspace::Table, CodeWorkspace::Id),
        )
        .check(
            Expr::col(CodeSession::Lifecycle)
                .is_in(["created", "idle", "running", "fenced", "ended"]),
        )
        .check(Expr::col(CodeSession::Kind).is_in(["interactive", "watch"]))
        .check(Expr::col(CodeSession::PermissionMode).is_in(["plan", "ask", "auto", "allow"]))
        .check(Expr::col(CodeSession::SpawnEpoch).gte(0))
        .check(Expr::col(CodeSession::UnrecognizedEventCount).gte(0))
        .to_owned()
}

pub(super) fn code_session_indexes() -> Vec<IndexCreateStatement> {
    vec![Index::create()
        .name("idx_code_session_workspace")
        .table(CodeSession::Table)
        .col(CodeSession::WorkspaceId)
        .to_owned()]
}

pub(super) fn code_turn_table() -> TableCreateStatement {
    Table::create()
        .table(CodeTurn::Table)
        .col(ColumnDef::new(CodeTurn::Id).uuid().not_null().primary_key())
        .col(owner_column(CodeTurn::Owner))
        .col(ColumnDef::new(CodeTurn::SessionId).uuid().not_null())
        .col(ColumnDef::new(CodeTurn::Ordinal).big_integer().not_null())
        .col(ColumnDef::new(CodeTurn::Status).text().not_null())
        .col(ColumnDef::new(CodeTurn::UserInput).text().not_null())
        .col(ColumnDef::new(CodeTurn::UserInputBlobId).uuid())
        .col(ColumnDef::new(CodeTurn::CheckpointRef).text())
        .col(ColumnDef::new(CodeTurn::Diffstat).json_binary())
        .col(ColumnDef::new(CodeTurn::Usage).json_binary())
        .col(ColumnDef::new(CodeTurn::Narrative).text())
        .col(
            ColumnDef::new(CodeTurn::StartedAt)
                .timestamp_with_time_zone()
                .not_null(),
        )
        .col(ColumnDef::new(CodeTurn::EndedAt).timestamp_with_time_zone())
        .foreign_key(
            ForeignKey::create()
                .name("fk_code_turn_session")
                .from(CodeTurn::Table, CodeTurn::SessionId)
                .to(CodeSession::Table, CodeSession::Id),
        )
        .check(Expr::col(CodeTurn::Status).is_in(["running", "completed", "failed", "interrupted"]))
        .check(Expr::col(CodeTurn::Ordinal).gte(1))
        .to_owned()
}

pub(super) fn code_turn_indexes() -> Vec<IndexCreateStatement> {
    vec![Index::create()
        .name("idx_code_turn_session_ordinal")
        .table(CodeTurn::Table)
        .col(CodeTurn::SessionId)
        .col(CodeTurn::Ordinal)
        .unique()
        .to_owned()]
}

/// Image references on a code-mode user turn. Another class of live blob
/// reference: these rows pin bytes the journal only names.
pub(super) fn code_turn_attachment_table() -> TableCreateStatement {
    Table::create()
        .table(CodeTurnAttachment::Table)
        .col(owner_column(CodeTurnAttachment::Owner))
        .col(ColumnDef::new(CodeTurnAttachment::TurnId).uuid().not_null())
        .col(
            ColumnDef::new(CodeTurnAttachment::Ordinal)
                .integer()
                .not_null(),
        )
        .col(ColumnDef::new(CodeTurnAttachment::BlobId).uuid().not_null())
        .col(
            ColumnDef::new(CodeTurnAttachment::MediaType)
                .string_len(64)
                .not_null(),
        )
        .col(
            ColumnDef::new(CodeTurnAttachment::ByteLen)
                .big_integer()
                .not_null(),
        )
        .primary_key(
            Index::create()
                .col(CodeTurnAttachment::TurnId)
                .col(CodeTurnAttachment::Ordinal),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_code_turn_attachment_turn")
                .from(CodeTurnAttachment::Table, CodeTurnAttachment::TurnId)
                .to(CodeTurn::Table, CodeTurn::Id)
                .on_delete(ForeignKeyAction::Cascade),
        )
        .check(Expr::col(CodeTurnAttachment::BlobId).ne(uuid::Uuid::nil()))
        .check(Expr::col(CodeTurnAttachment::Ordinal).gte(0))
        .check(
            Expr::col(CodeTurnAttachment::Ordinal).lt(crate::model::MAX_MESSAGE_ATTACHMENTS as i32),
        )
        .check(Expr::col(CodeTurnAttachment::MediaType).is_in([
            crate::image::ImageMediaType::Png.as_str(),
            crate::image::ImageMediaType::Jpeg.as_str(),
            crate::image::ImageMediaType::Webp.as_str(),
            crate::image::ImageMediaType::Gif.as_str(),
        ]))
        .check(
            Expr::col(CodeTurnAttachment::ByteLen).between(1, crate::image::MAX_IMAGE_BYTES as i64),
        )
        .to_owned()
}

pub(super) fn code_turn_attachment_indexes() -> Vec<IndexCreateStatement> {
    vec![Index::create()
        .name("idx_code_turn_attachment_blob")
        .table(CodeTurnAttachment::Table)
        .col(CodeTurnAttachment::BlobId)
        .to_owned()]
}

pub(super) fn code_event_table() -> TableCreateStatement {
    Table::create()
        .table(CodeEvent::Table)
        .col(owner_column(CodeEvent::Owner))
        .col(ColumnDef::new(CodeEvent::SessionId).uuid().not_null())
        .col(ColumnDef::new(CodeEvent::Seq).big_integer().not_null())
        .col(ColumnDef::new(CodeEvent::Event).json_binary().not_null())
        .col(
            ColumnDef::new(CodeEvent::CreatedAt)
                .timestamp_with_time_zone()
                .not_null(),
        )
        .primary_key(
            Index::create()
                .col(CodeEvent::SessionId)
                .col(CodeEvent::Seq),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_code_event_session")
                .from(CodeEvent::Table, CodeEvent::SessionId)
                .to(CodeSession::Table, CodeSession::Id),
        )
        .check(Expr::col(CodeEvent::Seq).gte(1))
        .to_owned()
}

pub(super) fn code_event_indexes() -> Vec<IndexCreateStatement> {
    vec![]
}

pub(super) fn code_approval_table() -> TableCreateStatement {
    Table::create()
        .table(CodeApproval::Table)
        .col(
            ColumnDef::new(CodeApproval::Id)
                .uuid()
                .not_null()
                .primary_key(),
        )
        .col(owner_column(CodeApproval::Owner))
        .col(ColumnDef::new(CodeApproval::SessionId).uuid().not_null())
        .col(ColumnDef::new(CodeApproval::TurnId).uuid().not_null())
        .col(ColumnDef::new(CodeApproval::Kind).json_binary().not_null())
        .col(
            ColumnDef::new(CodeApproval::HarnessRaw)
                .json_binary()
                .not_null(),
        )
        .col(ColumnDef::new(CodeApproval::State).text().not_null())
        .col(ColumnDef::new(CodeApproval::Feedback).text())
        .col(
            ColumnDef::new(CodeApproval::RequestedAt)
                .timestamp_with_time_zone()
                .not_null(),
        )
        .col(ColumnDef::new(CodeApproval::DecidedAt).timestamp_with_time_zone())
        .foreign_key(
            ForeignKey::create()
                .name("fk_code_approval_session")
                .from(CodeApproval::Table, CodeApproval::SessionId)
                .to(CodeSession::Table, CodeSession::Id),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_code_approval_turn")
                .from(CodeApproval::Table, CodeApproval::TurnId)
                .to(CodeTurn::Table, CodeTurn::Id),
        )
        .check(Expr::col(CodeApproval::State).is_in(["pending", "approved", "denied"]))
        .to_owned()
}

pub(super) fn code_approval_indexes() -> Vec<IndexCreateStatement> {
    vec![Index::create()
        .name("idx_code_approval_session_state")
        .table(CodeApproval::Table)
        .col(CodeApproval::SessionId)
        .col(CodeApproval::State)
        .to_owned()]
}

/// Durable watch tasks. One active watch per workspace, enforced in the
/// create path rather than by a partial index so terminal rows keep the
/// history.
pub(super) fn code_watch_table() -> TableCreateStatement {
    Table::create()
        .table(CodeWatch::Table)
        .col(
            ColumnDef::new(CodeWatch::Id)
                .uuid()
                .not_null()
                .primary_key(),
        )
        .col(owner_column(CodeWatch::Owner))
        .col(ColumnDef::new(CodeWatch::WorkspaceId).uuid().not_null())
        .col(ColumnDef::new(CodeWatch::SessionId).uuid().not_null())
        .col(ColumnDef::new(CodeWatch::PrNumber).big_integer().not_null())
        .col(ColumnDef::new(CodeWatch::State).text().not_null())
        .col(ColumnDef::new(CodeWatch::Detail).text())
        .col(ColumnDef::new(CodeWatch::LastFixHead).text())
        .col(
            ColumnDef::new(CodeWatch::Cycles)
                .big_integer()
                .not_null()
                .default(0),
        )
        .col(
            ColumnDef::new(CodeWatch::CreatedAt)
                .timestamp_with_time_zone()
                .not_null(),
        )
        .col(
            ColumnDef::new(CodeWatch::UpdatedAt)
                .timestamp_with_time_zone()
                .not_null(),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_code_watch_workspace")
                .from(CodeWatch::Table, CodeWatch::WorkspaceId)
                .to(CodeWorkspace::Table, CodeWorkspace::Id),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_code_watch_session")
                .from(CodeWatch::Table, CodeWatch::SessionId)
                .to(CodeSession::Table, CodeSession::Id),
        )
        .check(Expr::col(CodeWatch::State).is_in([
            "watching",
            "fixing",
            "blocked",
            "done",
            "stopped",
            "failed",
        ]))
        .check(Expr::col(CodeWatch::PrNumber).gte(1))
        .check(Expr::col(CodeWatch::Cycles).gte(0))
        .to_owned()
}

pub(super) fn code_watch_indexes() -> Vec<IndexCreateStatement> {
    vec![Index::create()
        .name("idx_code_watch_workspace")
        .table(CodeWatch::Table)
        .col(CodeWatch::WorkspaceId)
        .to_owned()]
}
