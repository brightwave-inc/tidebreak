//! Shared test fixtures for the remote modules: a seeded store with a repo,
//! workspace, session, and an activated incarnation.

use tidebreak_core::{
    Attention, AttentionSource, CodeSession, CodeSessionId, CodeSessionKind, CodeSessionLifecycle,
    HarnessKind, OwnerId, PermissionMode, WorkspaceId,
};

/// A session value with sensible remote defaults, unsaved.
pub(crate) fn session_value() -> CodeSession {
    CodeSession {
        id: CodeSessionId::new(),
        owner: OwnerId::local(),
        workspace_id: Some(WorkspaceId::new()),
        kind: CodeSessionKind::Interactive,
        harness_kind: HarnessKind::ClaudeCode,
        harness_version: None,
        harness_resume_ref: None,
        permission_mode: PermissionMode::Allow,
        model: None,
        reasoning_effort: None,
        fast_mode: false,
        lifecycle: CodeSessionLifecycle::Running,
        fence_reason: None,
        child_pid: None,
        child_process_identity: None,
        spawn_epoch: 1,
        attention: Attention::working(AttentionSource::Lifecycle),
        unrecognized_event_count: 0,
        subagents: Vec::new(),
        created_at: chrono::Utc::now(),
    }
}
