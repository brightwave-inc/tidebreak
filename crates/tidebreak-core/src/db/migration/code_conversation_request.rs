//! Durable conversation-tool request jobs for the channel adapter.
//!
//! One row per native tool call that waits on Slack payload provenance.
//! The row stores the exact operation and arguments, an idempotent
//! `call_key` scoped to one `(owner, session, grant, binding)`, and the
//! adapter's bounded result once completed. `created_at` is the TTL: the
//! adapter surface never returns an expired request and a stale request
//! cannot be completed.

use sea_orm_migration::prelude::*;

use super::idens;

pub(super) struct CodeConversationRequests;

impl MigrationName for CodeConversationRequests {
    fn name(&self) -> &str {
        "m20260910_000001_code_conversation_requests"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for CodeConversationRequests {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(idens::CodeConversationRequest::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(idens::CodeConversationRequest::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeConversationRequest::Owner)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeConversationRequest::SessionId)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeConversationRequest::GrantId)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeConversationRequest::BindingId)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeConversationRequest::CallKey)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeConversationRequest::Operation)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeConversationRequest::Arguments)
                            .json_binary()
                            .not_null(),
                    )
                    .col(ColumnDef::new(idens::CodeConversationRequest::Result).json_binary())
                    .col(
                        ColumnDef::new(idens::CodeConversationRequest::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeConversationRequest::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_code_conversation_request_session")
                            .from(
                                idens::CodeConversationRequest::Table,
                                idens::CodeConversationRequest::SessionId,
                            )
                            .to(idens::Session::Table, idens::Session::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_code_conversation_request_grant")
                            .from(
                                idens::CodeConversationRequest::Table,
                                idens::CodeConversationRequest::GrantId,
                            )
                            .to(
                                idens::CodeExternalGrant::Table,
                                idens::CodeExternalGrant::Id,
                            ),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_code_conversation_request_binding")
                            .from(
                                idens::CodeConversationRequest::Table,
                                idens::CodeConversationRequest::BindingId,
                            )
                            .to(
                                idens::CodeExternalBinding::Table,
                                idens::CodeExternalBinding::Id,
                            ),
                    )
                    .to_owned(),
            )
            .await?;
        // The replay gate: one tool call per owner/session/grant/binding.
        manager
            .create_index(
                Index::create()
                    .name("ix_code_conversation_request_call_key")
                    .table(idens::CodeConversationRequest::Table)
                    .col(idens::CodeConversationRequest::Owner)
                    .col(idens::CodeConversationRequest::SessionId)
                    .col(idens::CodeConversationRequest::GrantId)
                    .col(idens::CodeConversationRequest::BindingId)
                    .col(idens::CodeConversationRequest::CallKey)
                    .unique()
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;
        // The pending poll: unexpired, unanswered jobs in channel order.
        manager
            .create_index(
                Index::create()
                    .name("ix_code_conversation_request_pending")
                    .table(idens::CodeConversationRequest::Table)
                    .col(idens::CodeConversationRequest::Owner)
                    .col(idens::CodeConversationRequest::SessionId)
                    .col(idens::CodeConversationRequest::GrantId)
                    .col(idens::CodeConversationRequest::BindingId)
                    .col(idens::CodeConversationRequest::CreatedAt)
                    .col(idens::CodeConversationRequest::Id)
                    .and_where(Expr::col(idens::CodeConversationRequest::Result).is_null())
                    .if_not_exists()
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Dropping the table would forget in-flight adapter jobs; a rolled
        // back release leaves them readable by the same grant.
        Ok(())
    }
}
