//! Shared test fixtures for the remote modules: a seeded store with a repo,
//! workspace, session, and an activated incarnation.

use std::path::Path;
use std::sync::Arc;

use tidebreak_core::db::code::{
    activate_incarnation, append_event, clear_session_harness_resume_ref,
    create_incarnation_intent, insert_repo, insert_session, insert_workspace, reap_fenced_session,
    recover_interrupted_session, replace_session_attention, save_session, set_session_subagents,
};
use tidebreak_core::{
    Attention, AttentionSource, AttentionState, CapLevel, CodeIncarnationId, CodeRepo,
    CodeSubagentStatus, CodeWorkspace, CodeWorkspaceStatus, DbStore, Event, FenceReason,
    HarnessKind, IncarnationAdmission, OwnerId, PermissionMode, RepoId, Session, SessionId,
    SessionKind, SessionLifecycle, WorkspaceId,
};

use super::{replace_attention, RemoteReapError, RemoteSessionHost};

#[derive(Default)]
pub(crate) struct TestEvents;

#[async_trait::async_trait]
impl RemoteSessionHost for TestEvents {
    fn publish(&self, _session: SessionId, _event: tidebreak_core::code::SequencedEvent) {}

    async fn persist_session(
        &self,
        store: &DbStore,
        session: &Session,
    ) -> Result<bool, tidebreak_core::AgentError> {
        let saved = save_session(store, session).await?;
        if saved {
            let _ = replace_session_attention(
                store,
                &session.owner,
                session.id,
                &session.attention,
                false,
            )
            .await?;
        }
        Ok(saved)
    }

    async fn apply_attention(
        &self,
        store: &DbStore,
        owner: &OwnerId,
        session_id: SessionId,
        next: Attention,
    ) -> Result<(), tidebreak_core::AgentError> {
        let _ = replace_session_attention(store, owner, session_id, &next, false).await?;
        Ok(())
    }

    async fn journal_event(
        &self,
        store: &DbStore,
        owner: &OwnerId,
        session_id: SessionId,
        spawn_epoch: i64,
        event: Event,
    ) {
        let _ = append_event(store, owner, session_id, spawn_epoch, &event).await;
    }

    async fn fence_session(
        &self,
        store: &DbStore,
        session: &mut Session,
        reason: FenceReason,
    ) -> Result<(), tidebreak_core::AgentError> {
        if matches!(reason, FenceReason::ResumeLost { .. }) {
            session.harness_resume_ref = None;
            if !clear_session_harness_resume_ref(
                store,
                &session.owner,
                session.id,
                session.spawn_epoch,
            )
            .await?
            {
                return Err(tidebreak_core::AgentError::Store(format!(
                    "code session {} disappeared while clearing its rejected resume ref",
                    session.id
                )));
            }
        }
        session.lifecycle = SessionLifecycle::Fenced;
        for subagent in &mut session.subagents {
            if subagent.status == CodeSubagentStatus::Running {
                subagent.status = CodeSubagentStatus::Failed;
            }
        }
        session.fence_reason = Some(reason.clone());
        replace_attention(
            session,
            Attention::new(
                AttentionState::Fenced { reason },
                AttentionSource::Lifecycle,
            ),
            false,
        );
        if !save_session(store, session).await? {
            return Ok(());
        }
        let _ =
            replace_session_attention(store, &session.owner, session.id, &session.attention, false)
                .await?;
        let _ =
            set_session_subagents(store, &session.owner, session.id, &session.subagents).await?;
        Ok(())
    }

    async fn recover_dead_worker(
        &self,
        store: &DbStore,
        session: &Session,
    ) -> Result<Option<Session>, tidebreak_core::AgentError> {
        let durable_parks = if session.harness_kind.is_in_process() {
            CapLevel::Supported
        } else {
            CapLevel::Unsupported
        };
        Ok(recover_interrupted_session(
            store,
            &session.owner,
            session.id,
            session.spawn_epoch,
            durable_parks,
        )
        .await?
        .map(|recovered| recovered.session))
    }

    async fn reap_session(
        &self,
        store: &DbStore,
        session: Session,
    ) -> Result<Session, RemoteReapError> {
        let Some(recovered) = reap_fenced_session(
            store,
            &session.owner,
            session.id,
            session.spawn_epoch,
            session.child_pid,
            session.child_process_identity.as_deref(),
        )
        .await?
        else {
            return Err(RemoteReapError::Host(
                "the fenced session changed while it was being reaped".to_owned(),
            ));
        };
        Ok(recovered.session)
    }
}

/// A session value with sensible remote defaults, unsaved.
pub(crate) fn session_value() -> Session {
    Session {
        id: SessionId::new(),
        owner: OwnerId::local(),
        workspace_id: Some(WorkspaceId::new()),
        kind: SessionKind::Interactive,
        harness_kind: HarnessKind::ClaudeCode,
        harness_version: None,
        harness_resume_ref: None,
        permission_mode: PermissionMode::Allow,
        model: None,
        reasoning_effort: None,
        fast_mode: false,
        lifecycle: SessionLifecycle::Running,
        fence_reason: None,
        child_pid: None,
        child_process_identity: None,
        spawn_epoch: 1,
        attention: Attention::working(AttentionSource::Lifecycle),
        unrecognized_event_count: 0,
        subagents: Vec::new(),
        created_at: chrono::Utc::now(),
        execution_location: tidebreak_core::ExecutionLocation::Machine,
    }
}

/// A store with one repo (origin recorded), one remote workspace (no host
/// path), and one session, all inserted.
pub(crate) async fn seed(
    root: &Path,
) -> (Arc<DbStore>, TestEvents, Session, CodeWorkspace, CodeRepo) {
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
    (db, TestEvents, session, workspace, repo)
}

/// Reserve and activate one incarnation for the session.
pub(crate) async fn seeded_incarnation(db: &Arc<DbStore>, session: &Session) -> CodeIncarnationId {
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
