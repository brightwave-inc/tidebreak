//! Server-side orchestration for an external agent-engine session.
//!
//! Persistence lives in `tidebreak-core::db::code`. Protocol translation lives
//! in `tidebreak-harness`. This module owns git worktrees, the per-session
//! worker, crash recovery, and the live event bus.

pub mod approval_bridge;
pub mod approval_sweep;
pub mod attention;
pub mod browser_channel;
pub mod browser_runtime;
pub mod bus;
pub mod checkpoint;
pub mod ci_logs;
pub mod clone;
pub mod delivery;
pub mod forge_rest;
pub mod fork;
pub mod gh;
pub mod grants;
pub mod harness_install;
pub mod harness_llm;
pub mod harness_release;
pub mod memory;
pub mod memory_capture;
pub mod naming_settings;
pub mod pr_facts;
pub mod pr_fetch;
pub mod pr_refresh;
pub mod recap;
pub mod reconcile;
pub mod recovery;
pub mod remote;
pub mod rewrite;
pub mod runtime;
pub mod scoped;
pub mod scratch;
pub mod session_worker;
pub mod setup_script;
pub mod terminal;
pub mod titling;
pub mod trigger;
pub mod types;
pub mod watch;
pub mod worktree;
pub mod worktree_orphans;
pub mod worktree_root;

pub use runtime::CodeRuntime;
pub use scoped::ScopedCode;

/// Timestamp used to rank sessions for trigger delivery and its UI preview.
///
/// A session with turns ranks by its newest turn start. A session with no
/// turns ranks by creation time.
pub fn trigger_target_at(
    session_created_at: chrono::DateTime<chrono::Utc>,
    latest_turn_started_at: Option<chrono::DateTime<chrono::Utc>>,
) -> chrono::DateTime<chrono::Utc> {
    latest_turn_started_at.unwrap_or(session_created_at)
}

/// The workspace a session binds, when it binds one.
///
/// A session with no workspace (the in-process engine's) answers `None`
/// rather than a missing-row error, so callers that need a checkout can
/// skip the work instead of failing the session.
pub async fn session_workspace(
    db: &tidebreak_core::db::DbStore,
    session: &tidebreak_core::Session,
) -> Result<Option<tidebreak_core::CodeWorkspace>, tidebreak_core::AgentError> {
    match session.workspace_id {
        Some(workspace_id) => {
            tidebreak_core::db::code::get_workspace(db, &session.owner, workspace_id).await
        }
        None => Ok(None),
    }
}

/// The display name the product uses for an engine, wherever copy names one.
pub fn harness_label(kind: tidebreak_core::HarnessKind) -> &'static str {
    match kind {
        tidebreak_core::HarnessKind::ClaudeCode => "Claude Code",
        tidebreak_core::HarnessKind::Codex => "Codex CLI",
        tidebreak_core::HarnessKind::Opencode => "opencode",
        tidebreak_core::HarnessKind::Grok => "Grok CLI",
        tidebreak_core::HarnessKind::Internal => "Tidebreak",
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::trigger_target_at;

    #[test]
    fn trigger_target_time_prefers_the_latest_turn_and_falls_back_to_creation() {
        let created = Utc.with_ymd_and_hms(2026, 8, 28, 9, 0, 0).unwrap();
        let latest_turn = Utc.with_ymd_and_hms(2026, 8, 29, 11, 0, 0).unwrap();

        assert_eq!(trigger_target_at(created, Some(latest_turn)), latest_turn);
        assert_eq!(trigger_target_at(created, None), created);
    }
}
