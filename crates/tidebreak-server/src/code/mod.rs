//! Server-side orchestration for an external agent-engine session.
//!
//! Persistence lives in `tidebreak-core::db::code`. Protocol translation lives
//! in `tidebreak-harness`. This module owns git worktrees, the per-session
//! worker, crash recovery, and the live event bus.

pub(crate) mod approval_bridge;
pub(crate) mod approval_sweep;
pub(crate) mod attention;
pub(crate) mod browser_channel;
pub(crate) mod browser_runtime;
pub(crate) mod bus;
pub(crate) mod checkpoint;
pub(crate) mod ci_logs;
pub(crate) mod clone;
pub(crate) mod delivery;
pub(crate) mod forge_rest;
pub(crate) mod fork;
pub(crate) mod gh;
pub(crate) mod grants;
pub(crate) mod harness_install;
pub(crate) mod harness_llm;
pub(crate) mod naming_settings;
pub(crate) mod pr_facts;
pub(crate) mod pr_fetch;
pub(crate) mod pr_refresh;
pub(crate) mod recap;
pub(crate) mod reconcile;
pub(crate) mod recovery;
pub(crate) mod remote;
pub(crate) mod rewrite;
pub(crate) mod runtime;
pub(crate) mod scoped;
pub(crate) mod scratch;
pub(crate) mod session_worker;
pub(crate) mod setup_script;
pub(crate) mod terminal;
pub(crate) mod titling;
pub(crate) mod trigger;
pub(crate) mod watch;
pub(crate) mod worktree;
pub(crate) mod worktree_orphans;
pub(crate) mod worktree_root;

pub(crate) use runtime::CodeRuntime;
pub(crate) use scoped::ScopedCode;

/// Timestamp used to rank sessions for trigger delivery and its UI preview.
///
/// A session with turns ranks by its newest turn start. A session with no
/// turns ranks by creation time.
pub(crate) fn trigger_target_at(
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
pub(crate) async fn session_workspace(
    db: &tidebreak_core::db::DbStore,
    session: &tidebreak_core::CodeSession,
) -> Result<Option<tidebreak_core::CodeWorkspace>, tidebreak_core::AgentError> {
    match session.workspace_id {
        Some(workspace_id) => {
            tidebreak_core::db::code::get_workspace(db, &session.owner, workspace_id).await
        }
        None => Ok(None),
    }
}

/// The display name the product uses for an engine, wherever copy names one.
pub(crate) fn harness_label(kind: tidebreak_core::HarnessKind) -> &'static str {
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
