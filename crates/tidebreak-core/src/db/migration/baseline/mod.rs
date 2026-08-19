//! The schema baseline: every table, index, and seed row a fresh database
//! starts with.
//!
//! Pre-v1 desktop databases are disposable: `tidebreak-server`'s schema-epoch
//! guard discards a database written by an older baseline. The self-host
//! PostgreSQL store is durable, so a renamed or edited baseline must not
//! recreate existing tables, and a later in-place edit that changes an
//! existing table must also have an ordered upgrade migration in
//! [`crate::db::migration`]. Squash this snapshot again before `1.0.0`.

use sea_orm_migration::prelude::*;

mod agent_run;
mod chat;
mod code;
mod content;
mod sandbox;
mod tools;
mod turn;

/// The seed rows the baseline inserts: one `advisory_lock` row per claim path,
/// whose presence is what serializes it. The names come from
/// [`crate::db::ops::AdvisoryLockName`], which is what the acquire path filters
/// on.
pub(super) const SEED_STATEMENTS: &[&str] = &[
    "INSERT INTO advisory_lock (name) VALUES ('turn_claim') ON CONFLICT DO NOTHING",
    "INSERT INTO advisory_lock (name) VALUES ('agent_run_claim') ON CONFLICT DO NOTHING",
    "INSERT INTO advisory_lock (name) VALUES ('turn_agent_run_wait') ON CONFLICT DO NOTHING",
];

/// A table and the named indexes that belong to it. Implicit indexes come
/// from the primary-key and unique column definitions in the table itself.
pub(crate) struct BaselineTable {
    pub(crate) table: TableCreateStatement,
    pub(super) indexes: Vec<IndexCreateStatement>,
}

/// Every table, ordered so each one's foreign keys point only at tables that
/// already exist — and, on Postgres, only at unique constraints or unique
/// indexes that already exist, which is why each table's indexes are created
/// with it rather than in a second pass. `down` drops them in reverse.
#[cfg(test)]
pub(crate) fn tables_for_test() -> Vec<BaselineTable> {
    tables()
}

pub(super) fn tables() -> Vec<BaselineTable> {
    vec![
        // Chats and their history.
        entry(chat::project_table(), chat::project_indexes()),
        entry(chat::setting_table(), chat::setting_indexes()),
        entry(chat::chat_table(), chat::chat_indexes()),
        entry(
            chat::project_root_attachment_table(),
            chat::project_root_attachment_indexes(),
        ),
        entry(
            chat::chat_root_attachment_table(),
            chat::chat_root_attachment_indexes(),
        ),
        entry(
            chat::root_attachment_change_table(),
            chat::root_attachment_change_indexes(),
        ),
        entry(
            chat::message_identity_table(),
            chat::message_identity_indexes(),
        ),
        entry(chat::message_table(), chat::message_indexes()),
        entry(
            chat::blob_retirement_table(),
            chat::blob_retirement_indexes(),
        ),
        // The advisory locks every claim path serializes on.
        entry(turn::advisory_lock_table(), turn::advisory_lock_indexes()),
        // Turn scheduling.
        entry(turn::turn_claim_table(), turn::turn_claim_indexes()),
        entry(turn::turn_failure_table(), turn::turn_failure_indexes()),
        // Agent runs.
        entry(
            agent_run::agent_run_claim_table(),
            agent_run::agent_run_claim_indexes(),
        ),
        entry(agent_run::agent_run_table(), agent_run::agent_run_indexes()),
        entry(
            agent_run::agent_run_result_table(),
            agent_run::agent_run_result_indexes(),
        ),
        entry(
            agent_run::agent_run_cancellation_table(),
            agent_run::agent_run_cancellation_indexes(),
        ),
        entry(
            agent_run::agent_run_progress_table(),
            agent_run::agent_run_progress_indexes(),
        ),
        // Turn runs and the journal.
        entry(turn::turn_admission_table(), turn::turn_admission_indexes()),
        entry(turn::turn_run_table(), turn::turn_run_indexes()),
        entry(turn::queued_turn_table(), turn::queued_turn_indexes()),
        entry(chat::event_table(), chat::event_indexes()),
        entry(
            agent_run::agent_run_inbox_table(),
            agent_run::agent_run_inbox_indexes(),
        ),
        entry(turn::turn_steer_table(), turn::turn_steer_indexes()),
        // Tool calls and what parks on them.
        entry(tools::tool_call_table(), tools::tool_call_indexes()),
        entry(
            tools::standing_tool_grant_table(),
            tools::standing_tool_grant_indexes(),
        ),
        entry(tools::operation_log_table(), tools::operation_log_indexes()),
        entry(tools::plan_request_table(), tools::plan_request_indexes()),
        entry(tools::task_plan_table(), tools::task_plan_indexes()),
        entry(
            tools::user_question_request_table(),
            tools::user_question_request_indexes(),
        ),
        entry(tools::user_question_table(), tools::user_question_indexes()),
        entry(
            turn::turn_client_wait_table(),
            turn::turn_client_wait_indexes(),
        ),
        entry(
            turn::context_checkpoint_table(),
            turn::context_checkpoint_indexes(),
        ),
        // Documents, outputs, and attachments.
        entry(content::document_table(), content::document_indexes()),
        entry(content::output_table(), content::output_indexes()),
        entry(
            content::output_revision_table(),
            content::output_revision_indexes(),
        ),
        entry(
            content::assistant_citation_table(),
            content::assistant_citation_indexes(),
        ),
        entry(
            content::chat_image_publication_table(),
            content::chat_image_publication_indexes(),
        ),
        entry(
            content::message_attachment_table(),
            content::message_attachment_indexes(),
        ),
        entry(
            content::message_document_attachment_table(),
            content::message_document_attachment_indexes(),
        ),
        entry(content::app_table(), content::app_indexes()),
        entry(
            content::app_revision_table(),
            content::app_revision_indexes(),
        ),
        entry(content::app_grant_table(), content::app_grant_indexes()),
        entry(
            content::app_gateway_draft_table(),
            content::app_gateway_draft_indexes(),
        ),
        entry(
            content::connected_app_table(),
            content::connected_app_indexes(),
        ),
        // Sandboxed execution.
        entry(
            sandbox::sandbox_provision_table(),
            sandbox::sandbox_provision_indexes(),
        ),
        entry(
            sandbox::sandbox_tool_call_table(),
            sandbox::sandbox_tool_call_indexes(),
        ),
        entry(
            sandbox::sandbox_spawn_checkpoint_table(),
            sandbox::sandbox_spawn_checkpoint_indexes(),
        ),
        entry(
            sandbox::agent_run_task_plan_table(),
            sandbox::agent_run_task_plan_indexes(),
        ),
        entry(
            sandbox::exec_file_change_table(),
            sandbox::exec_file_change_indexes(),
        ),
        // Multi-agent wait sets, which reference the admitted child runs.
        entry(
            agent_run::turn_agent_run_wait_set_table(),
            agent_run::turn_agent_run_wait_set_indexes(),
        ),
        entry(
            agent_run::turn_agent_run_wait_member_table(),
            agent_run::turn_agent_run_wait_member_indexes(),
        ),
        // External agent-engine sessions. Isolated from chat tables.
        entry(code::code_repo_table(), code::code_repo_indexes()),
        entry(code::code_workspace_table(), code::code_workspace_indexes()),
        entry(code::code_session_table(), code::code_session_indexes()),
        entry(code::code_turn_table(), code::code_turn_indexes()),
        entry(
            code::code_turn_attachment_table(),
            code::code_turn_attachment_indexes(),
        ),
        entry(code::code_event_table(), code::code_event_indexes()),
        entry(code::code_approval_table(), code::code_approval_indexes()),
    ]
}

fn entry(table: TableCreateStatement, indexes: Vec<IndexCreateStatement>) -> BaselineTable {
    BaselineTable { table, indexes }
}
