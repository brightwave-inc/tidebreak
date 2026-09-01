//! Shared test fixtures for the remote modules: a seeded store with a repo,
//! workspace, session, and an activated incarnation.

use std::path::Path;
use std::sync::Arc;

use tidebreak_core::db::code::{
    activate_incarnation, create_incarnation_intent, insert_repo, insert_session, insert_workspace,
};
use tidebreak_core::{
    Attention, AttentionSource, CodeIncarnationId, CodeRepo, CodeSession, CodeSessionId,
    CodeSessionKind, CodeSessionLifecycle, CodeWorkspace, CodeWorkspaceStatus, DbStore,
    HarnessKind, IncarnationAdmission, OwnerId, PermissionMode, RepoId, WorkspaceId,
};

use super::super::bus::CodeEventBus;

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

/// A store with one repo (origin recorded), one remote workspace (no host
/// path), and one session, all inserted.
pub(crate) async fn seed(
    root: &Path,
) -> (
    Arc<DbStore>,
    CodeEventBus,
    CodeSession,
    CodeWorkspace,
    CodeRepo,
) {
    let db = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            root.join("code.db").display()
        ))
        .await
        .unwrap(),
    );
    let owner = OwnerId::local();
    let repo = CodeRepo {
        id: RepoId::new(),
        owner: owner.clone(),
        root_path: root.join("repo").display().to_string(),
        display_name: "example".into(),
        default_base_ref: "main".into(),
        branch_prefix: "tidebreak/".into(),
        setup_script: None,
        archive_script: None,
        quick_actions: Vec::new(),
        created_at: chrono::Utc::now(),
        removed_at: None,
        cloned_from: None,
        origin_host: Some("github.com".into()),
        origin_owner: Some("acme".into()),
        origin_name: Some("tools".into()),
    };
    insert_repo(&db, &repo).await.unwrap();
    let workspace = CodeWorkspace {
        id: WorkspaceId::new(),
        owner: owner.clone(),
        repo_id: repo.id,
        title: "remote".into(),
        worktree_path: String::new(),
        branch_name: "tidebreak/remote".into(),
        base_ref: "main".into(),
        status: CodeWorkspaceStatus::Active,
        pr: None,
        created_at: chrono::Utc::now(),
        archived_at: None,
        released_at: None,
        released_tip: None,
        bundle_bytes: None,
    };
    insert_workspace(&db, &workspace).await.unwrap();
    let mut session = session_value();
    session.workspace_id = Some(workspace.id);
    insert_session(&db, &session).await.unwrap();
    (db, CodeEventBus::default(), session, workspace, repo)
}

/// Reserve and activate one incarnation for the session.
pub(crate) async fn seeded_incarnation(
    db: &Arc<DbStore>,
    session: &CodeSession,
) -> CodeIncarnationId {
    let admission = create_incarnation_intent(db, &session.owner, session.id, 1, 4)
        .await
        .unwrap();
    let IncarnationAdmission::Admitted(row) = admission else {
        panic!("expected admission");
    };
    activate_incarnation(db, &session.owner, row.id, "sb-1")
        .await
        .unwrap();
    row.id
}
