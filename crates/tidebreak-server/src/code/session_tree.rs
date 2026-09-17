//! Direct child trees for a parent session snapshot and journal.
//!
//! Slack's parent follower reads `SessionSnapshot.children` / `wait` on
//! connect and `Event::SessionTree` while it watches. The tree is bounded to
//! same-owner direct children; an adapter grant sees only children bound to
//! that grant. `wait` is present only when a parent wait is actually known.

use tidebreak_core::db::code::{
    append_event, child_sessions, get_session, get_workspace, latest_turn, list_queued_turns,
    parent_wait_ids, session_bound_to_grant, session_context, set_parent_wait_ids,
};
use tidebreak_core::{
    AttentionState, CodeGrantId, DbStore, Event, OwnerId, Session, SessionId, SessionLifecycle,
    SessionTreeChild, SessionTreeChildStatus, SessionTreeWait, TurnStatus,
};

use super::bus::CodeEventBus;
use super::types::SessionSnapshot;

/// Authoritative wait for a parent digest. Never inferred from running children.
pub async fn wait_for_parent(
    db: &DbStore,
    owner: &OwnerId,
    parent_id: SessionId,
) -> Option<SessionTreeWait> {
    compute(db, owner, parent_id, None)
        .await
        .ok()
        .and_then(|(_, wait)| wait)
}

/// Fill `children` / `wait` on a snapshot from persisted child rows.
pub async fn attach_to_snapshot(
    db: &DbStore,
    owner: &OwnerId,
    snapshot: &mut SessionSnapshot,
    grant_id: Option<CodeGrantId>,
    principal: Option<&OwnerId>,
) {
    match compute(db, owner, snapshot.id, grant_id).await {
        Ok((children, wait)) => {
            let event = authorize_event(
                db,
                owner,
                None,
                principal,
                Event::SessionTree { children, wait },
            )
            .await;
            if let Event::SessionTree { children, wait } = event {
                snapshot.children = children;
                snapshot.wait = wait;
            }
        }
        Err(error) => {
            tracing::warn!(
                session = %snapshot.id,
                error = %error,
                "could not attach the session tree to a snapshot"
            );
        }
    }
}

/// Keep only children the adapter grant or shared-session reader can read.
pub async fn authorize_event(
    db: &DbStore,
    owner: &OwnerId,
    grant_id: Option<CodeGrantId>,
    principal: Option<&OwnerId>,
    event: Event,
) -> Event {
    let Event::SessionTree { children, wait } = event else {
        return event;
    };
    let original_count = children.len();
    let mut children = match grant_id {
        Some(grant_id) => filter_bound_children(db, owner, grant_id, children).await,
        None => children,
    };
    if let Some(principal) = principal {
        let mut visible = Vec::with_capacity(children.len());
        for child in children {
            if tidebreak_core::db::code::resolve_session_access(db, principal, child.id)
                .await
                .ok()
                .flatten()
                .is_some()
            {
                visible.push(child);
            }
        }
        children = visible;
    }
    Event::SessionTree {
        wait: if children.len() == original_count {
            wait
        } else {
            None
        },
        children,
    }
}

/// Publish a parent `session_tree` event when this session is a child.
pub async fn publish_for_child(db: &DbStore, bus: &CodeEventBus, child: &Session) {
    match session_context(db, &child.owner, child.id)
        .await
        .map(|context| context.and_then(|context| context.parent_session_id))
    {
        Ok(Some(parent_id)) => publish_for_parent(db, bus, &child.owner, parent_id).await,
        Ok(None) => {}
        Err(error) => {
            tracing::warn!(
                session = %child.id,
                error = %error,
                "could not look up a child session's parent"
            );
        }
    }
}

/// Restate the parent's direct children on its journal when that set changed.
pub async fn publish_for_parent(
    db: &DbStore,
    bus: &CodeEventBus,
    owner: &OwnerId,
    parent_id: SessionId,
) {
    let gate = bus.session_tree_gate(parent_id);
    let _guard = gate.lock().await;
    let (children, wait) = match compute(db, owner, parent_id, None).await {
        Ok(tree) => tree,
        Err(error) => {
            tracing::warn!(
                session = %parent_id,
                error = %error,
                "could not compute the session tree"
            );
            return;
        }
    };
    if bus.session_tree_is_current(parent_id, &children, &wait) {
        return;
    }
    let Some(parent) = get_session(db, owner, parent_id).await.ok().flatten() else {
        return;
    };
    let event = Event::SessionTree {
        children: children.clone(),
        wait: wait.clone(),
    };
    match append_event(db, owner, parent.id, parent.spawn_epoch, &event).await {
        Ok(seq) => {
            bus.remember_session_tree(parent.id, children, wait);
            bus.publish(
                parent.id,
                tidebreak_core::code::SequencedEvent { seq, event },
            );
        }
        Err(error) => {
            tracing::warn!(
                session = %parent.id,
                error = %error,
                "could not journal a session_tree update"
            );
        }
    }
}

pub(crate) fn child_status(
    lifecycle: SessionLifecycle,
    queued: bool,
    last_turn: Option<TurnStatus>,
) -> SessionTreeChildStatus {
    match lifecycle {
        SessionLifecycle::Fenced => SessionTreeChildStatus::Fenced,
        SessionLifecycle::Running => SessionTreeChildStatus::Running,
        SessionLifecycle::Created => SessionTreeChildStatus::Queued,
        SessionLifecycle::Idle if queued || last_turn.is_none() => SessionTreeChildStatus::Queued,
        SessionLifecycle::Idle | SessionLifecycle::Ended => match last_turn {
            Some(TurnStatus::Failed) => SessionTreeChildStatus::Failed,
            Some(TurnStatus::Interrupted) => SessionTreeChildStatus::Interrupted,
            Some(status) if status.is_open() => {
                if lifecycle == SessionLifecycle::Ended {
                    SessionTreeChildStatus::Interrupted
                } else {
                    SessionTreeChildStatus::Running
                }
            }
            _ => SessionTreeChildStatus::Completed,
        },
    }
}

fn child_needs_attention(session: &Session) -> bool {
    session.lifecycle == SessionLifecycle::Fenced
        || matches!(
            session.attention.state,
            AttentionState::NeedsYou { .. }
                | AttentionState::Fenced { .. }
                | AttentionState::Stalled { .. }
                | AttentionState::Manual { .. }
        )
}

fn child_is_fenced(session: &Session) -> bool {
    session.lifecycle == SessionLifecycle::Fenced
        || matches!(session.attention.state, AttentionState::Fenced { .. })
}

fn child_is_settled(status: SessionTreeChildStatus) -> bool {
    matches!(
        status,
        SessionTreeChildStatus::Completed
            | SessionTreeChildStatus::Failed
            | SessionTreeChildStatus::Interrupted
    )
}

/// Parent wait is never invented by counting every running child.
fn wait_from_named(
    children: &[SessionTreeChild],
    named: Option<&[SessionId]>,
) -> Option<SessionTreeWait> {
    let named = named.filter(|ids| !ids.is_empty())?;
    let total = u32::try_from(named.len()).unwrap_or(u32::MAX);
    let waiting = named
        .iter()
        .filter(|id| {
            children
                .iter()
                .find(|child| child.id == **id)
                .is_some_and(|child| !child_is_settled(child.status))
        })
        .count();
    let waiting = u32::try_from(waiting).unwrap_or(u32::MAX);
    (waiting > 0).then_some(SessionTreeWait { waiting, total })
}

async fn compute(
    db: &DbStore,
    owner: &OwnerId,
    parent_id: SessionId,
    grant_id: Option<CodeGrantId>,
) -> Result<(Vec<SessionTreeChild>, Option<SessionTreeWait>), tidebreak_core::AgentError> {
    let mut children = Vec::new();
    for session in child_sessions(db, owner, parent_id).await? {
        if let Some(grant_id) = grant_id {
            if !session_bound_to_grant(db, owner, session.id, grant_id).await? {
                continue;
            }
        }
        children.push(project_child(db, owner, session).await?);
    }
    let named = parent_wait_ids(db, owner, parent_id).await?;
    let wait = wait_from_named(&children, named.as_deref());
    if wait.is_none() && named.is_some() {
        set_parent_wait_ids(db, owner, parent_id, &[]).await?;
    }
    Ok((children, wait))
}

async fn project_child(
    db: &DbStore,
    owner: &OwnerId,
    session: Session,
) -> Result<SessionTreeChild, tidebreak_core::AgentError> {
    let queued = !list_queued_turns(db, owner, session.id).await?.is_empty();
    let last_turn = latest_turn(db, owner, session.id)
        .await?
        .map(|turn| turn.status);
    let title = match session.workspace_id {
        Some(workspace_id) => get_workspace(db, owner, workspace_id)
            .await?
            .map(|workspace| workspace.title)
            .filter(|title| !title.trim().is_empty()),
        None => None,
    };
    Ok(SessionTreeChild {
        id: session.id,
        title,
        status: child_status(session.lifecycle, queued, last_turn),
        attention: child_needs_attention(&session),
        fenced: child_is_fenced(&session),
        workspace_id: session.workspace_id,
        execution_location: Some(session.execution_location),
    })
}

async fn filter_bound_children(
    db: &DbStore,
    owner: &OwnerId,
    grant_id: CodeGrantId,
    children: Vec<SessionTreeChild>,
) -> Vec<SessionTreeChild> {
    let mut authorized = Vec::with_capacity(children.len());
    for child in children {
        match session_bound_to_grant(db, owner, child.id, grant_id).await {
            Ok(true) => authorized.push(child),
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(
                    session = %child.id,
                    error = %error,
                    "could not authorize a child on the session tree"
                );
            }
        }
    }
    authorized
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::attention::persist_session;
    use crate::code::runtime::CodeRuntime;
    use std::sync::Arc;
    use tidebreak_core::db::code::{insert_session, save_session, set_session_context};
    use tidebreak_core::{
        Attention, AttentionSource, ExecutionLocation, FenceReason, HarnessKind, PermissionMode,
        SessionKind, SessionVisibility,
    };

    fn session(owner: &OwnerId, lifecycle: SessionLifecycle) -> Session {
        Session {
            visibility: SessionVisibility::Private,
            id: SessionId::new(),
            owner: owner.clone(),
            owner_kind: None,
            workspace_id: None,
            kind: SessionKind::Interactive,
            harness_kind: HarnessKind::Internal,
            harness_version: None,
            harness_resume_ref: None,
            permission_mode: PermissionMode::Allow,
            model: None,
            reasoning_effort: None,
            fast_mode: false,
            lifecycle,
            fence_reason: None,
            child_pid: None,
            child_process_identity: None,
            spawn_epoch: 1,
            attention: Attention::working(AttentionSource::Lifecycle),
            unrecognized_event_count: 0,
            subagents: Vec::new(),
            created_at: chrono::Utc::now(),
            execution_location: ExecutionLocation::Machine,
            acts_as: None,
        }
    }

    async fn store() -> (tempfile::TempDir, DbStore) {
        let dir = tempfile::tempdir().unwrap();
        let db = DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("session-tree.db").display()
        ))
        .await
        .unwrap();
        (dir, db)
    }

    #[tokio::test]
    async fn shared_parent_does_not_disclose_private_children() {
        let (_dir, db) = store().await;
        let owner = OwnerId::local();
        let reader = OwnerId::new("other-reader").unwrap();
        let private = session(&owner, SessionLifecycle::Running);
        let mut visible = session(&owner, SessionLifecycle::Running);
        visible.visibility = SessionVisibility::Deployment;
        insert_session(&db, &private).await.unwrap();
        insert_session(&db, &visible).await.unwrap();
        let event = Event::SessionTree {
            children: vec![
                project_child(&db, &owner, private).await.unwrap(),
                project_child(&db, &owner, visible.clone()).await.unwrap(),
            ],
            wait: Some(SessionTreeWait {
                waiting: 2,
                total: 2,
            }),
        };
        let authorized = authorize_event(&db, &owner, None, Some(&reader), event).await;
        let Event::SessionTree { children, wait } = authorized else {
            panic!("expected session tree");
        };
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].id, visible.id);
        assert!(
            wait.is_none(),
            "wait counts must not disclose hidden children"
        );
    }

    #[test]
    fn child_status_does_not_invent_a_wait_and_maps_lifecycle() {
        assert_eq!(
            child_status(SessionLifecycle::Running, false, None),
            SessionTreeChildStatus::Running
        );
        assert_eq!(
            child_status(SessionLifecycle::Created, false, None),
            SessionTreeChildStatus::Queued
        );
        assert_eq!(
            child_status(SessionLifecycle::Idle, true, Some(TurnStatus::Completed)),
            SessionTreeChildStatus::Queued
        );
        assert_eq!(
            child_status(SessionLifecycle::Idle, false, None),
            SessionTreeChildStatus::Queued
        );
        assert_eq!(
            child_status(SessionLifecycle::Idle, false, Some(TurnStatus::Completed)),
            SessionTreeChildStatus::Completed
        );
        assert_eq!(
            child_status(SessionLifecycle::Ended, false, Some(TurnStatus::Failed)),
            SessionTreeChildStatus::Failed
        );
        assert_eq!(
            child_status(
                SessionLifecycle::Ended,
                false,
                Some(TurnStatus::Interrupted)
            ),
            SessionTreeChildStatus::Interrupted
        );
        assert_eq!(
            child_status(SessionLifecycle::Fenced, false, Some(TurnStatus::Running)),
            SessionTreeChildStatus::Fenced
        );
        assert!(wait_from_named(
            &[SessionTreeChild {
                id: SessionId::new(),
                title: None,
                status: SessionTreeChildStatus::Running,
                attention: false,
                fenced: false,
                workspace_id: None,
                execution_location: None,
            }],
            None
        )
        .is_none());
    }

    #[tokio::test]
    async fn snapshot_and_reconnect_recompute_direct_children_from_persisted_state() {
        let (_dir, db) = store().await;
        let owner = OwnerId::local();
        let parent = session(&owner, SessionLifecycle::Idle);
        let mut child = session(&owner, SessionLifecycle::Running);
        insert_session(&db, &parent).await.unwrap();
        insert_session(&db, &child).await.unwrap();
        set_session_context(&db, &owner, child.id, None, Some(parent.id), Some("one"))
            .await
            .unwrap();

        let (children, wait) = compute(&db, &owner, parent.id, None).await.unwrap();
        assert!(wait.is_none(), "running children must not invent a wait");
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].id, child.id);
        assert_eq!(children[0].status, SessionTreeChildStatus::Running);
        assert!(!children[0].fenced);
        assert_eq!(children[0].workspace_id, child.workspace_id);
        assert_eq!(
            children[0].execution_location,
            Some(ExecutionLocation::Machine)
        );

        child.lifecycle = SessionLifecycle::Fenced;
        child.fence_reason = Some(FenceReason::OrphanAlive);
        child.attention = Attention::new(
            AttentionState::Fenced {
                reason: FenceReason::OrphanAlive,
            },
            AttentionSource::Lifecycle,
        );
        save_session(&db, &child).await.unwrap();

        let (children, wait) = compute(&db, &owner, parent.id, None).await.unwrap();
        assert!(wait.is_none());
        assert_eq!(children[0].status, SessionTreeChildStatus::Fenced);
        assert!(children[0].fenced);
        assert!(children[0].attention);
    }

    #[tokio::test]
    async fn live_child_status_reaches_a_quiet_parent_journal() {
        let (_dir, db) = store().await;
        let bus = CodeEventBus::default();
        let owner = OwnerId::local();
        let parent = session(&owner, SessionLifecycle::Idle);
        let mut child = session(&owner, SessionLifecycle::Running);
        insert_session(&db, &parent).await.unwrap();
        insert_session(&db, &child).await.unwrap();
        set_session_context(&db, &owner, child.id, None, Some(parent.id), Some("one"))
            .await
            .unwrap();

        persist_session(&db, &bus, &child).await.unwrap();
        let first = tidebreak_core::db::code::list_events(&db, &owner, parent.id, 0, 20)
            .await
            .unwrap();
        assert!(
            first.events.iter().any(|entry| matches!(
                &entry.event,
                Event::SessionTree { children, wait }
                    if wait.is_none()
                        && children.len() == 1
                        && children[0].id == child.id
                        && children[0].status == SessionTreeChildStatus::Running
            )),
            "quiet parent journal missing the first tree: {first:?}"
        );

        child.lifecycle = SessionLifecycle::Fenced;
        child.fence_reason = Some(FenceReason::OrphanAlive);
        persist_session(&db, &bus, &child).await.unwrap();
        let page = tidebreak_core::db::code::list_events(&db, &owner, parent.id, 0, 20)
            .await
            .unwrap();
        assert!(
            page.events.iter().any(|entry| matches!(
                &entry.event,
                Event::SessionTree { children, wait }
                    if wait.is_none()
                        && children.len() == 1
                        && children[0].status == SessionTreeChildStatus::Fenced
            )),
            "child fence did not reach the parent journal: {page:?}"
        );
    }

    #[tokio::test]
    async fn ending_the_parent_leaves_children_running_and_on_the_tree() {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                dir.path().join("session-tree-end.db").display()
            ))
            .await
            .unwrap(),
        );
        let runtime = CodeRuntime::new(db, dir.path().into(), None, None, None, None, None, None);
        let owner = OwnerId::local();
        let parent = session(&owner, SessionLifecycle::Idle);
        let child = session(&owner, SessionLifecycle::Running);
        insert_session(&runtime.db, &parent).await.unwrap();
        insert_session(&runtime.db, &child).await.unwrap();
        set_session_context(
            &runtime.db,
            &owner,
            child.id,
            None,
            Some(parent.id),
            Some("one"),
        )
        .await
        .unwrap();

        runtime.end_session_row(&owner, parent.id).await.unwrap();
        let ended = runtime.get_session(&owner, parent.id).await.unwrap();
        assert_eq!(ended.lifecycle, SessionLifecycle::Ended);
        let live = runtime.get_session(&owner, child.id).await.unwrap();
        assert_eq!(live.lifecycle, SessionLifecycle::Running);
        let (children, wait) = compute(&runtime.db, &owner, parent.id, None).await.unwrap();
        assert!(wait.is_none());
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].id, child.id);
        assert_eq!(children[0].status, SessionTreeChildStatus::Running);
    }

    #[tokio::test]
    async fn grant_filter_omits_children_not_bound_to_the_viewer() {
        let (_dir, db) = store().await;
        let owner = OwnerId::local();
        let parent = session(&owner, SessionLifecycle::Idle);
        let visible = session(&owner, SessionLifecycle::Running);
        let hidden = session(&owner, SessionLifecycle::Running);
        insert_session(&db, &parent).await.unwrap();
        insert_session(&db, &visible).await.unwrap();
        insert_session(&db, &hidden).await.unwrap();
        set_session_context(
            &db,
            &owner,
            visible.id,
            None,
            Some(parent.id),
            Some("visible"),
        )
        .await
        .unwrap();
        set_session_context(
            &db,
            &owner,
            hidden.id,
            None,
            Some(parent.id),
            Some("hidden"),
        )
        .await
        .unwrap();

        let grant = tidebreak_core::db::code::mint_external_grant(
            &db,
            &owner,
            tidebreak_core::db::code::MintGrantSubject {
                channel_kind: "slack",
                external_identity: "U",
                workspace_identity: "W",
                kind: tidebreak_core::CodeGrantKind::Person,
            },
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        )
        .await
        .unwrap();
        tidebreak_core::db::code::bind_external_session(
            &db,
            &owner,
            grant.id,
            "slack",
            &format!("child/{}/visible", parent.id),
            visible.id,
        )
        .await
        .unwrap();

        let (children, wait) = compute(&db, &owner, parent.id, Some(grant.id))
            .await
            .unwrap();
        assert!(wait.is_none());
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].id, visible.id);

        let stranger = OwnerId::new("other").unwrap();
        let mut foreign = session(&stranger, SessionLifecycle::Running);
        foreign.owner = stranger.clone();
        insert_session(&db, &foreign).await.unwrap();
        let denied = set_session_context(
            &db,
            &stranger,
            foreign.id,
            None,
            Some(parent.id),
            Some("foreign"),
        )
        .await;
        assert!(denied.is_err(), "other-owner children must not link");
        let (children, _) = compute(&db, &owner, parent.id, None).await.unwrap();
        assert!(children.iter().all(|child| child.id != foreign.id));
    }

    #[test]
    fn empty_tree_snapshot_emits_children_array_and_null_wait() {
        let snapshot = SessionSnapshot::from(session(&OwnerId::local(), SessionLifecycle::Idle));
        let value = serde_json::to_value(&snapshot).unwrap();
        assert_eq!(value["children"], serde_json::json!([]));
        assert_eq!(value["wait"], serde_json::Value::Null);
        assert!(
            value.as_object().unwrap().contains_key("children"),
            "empty children must be present so reconnect can clear a stale tree"
        );
        assert!(
            value.as_object().unwrap().contains_key("wait"),
            "null wait must be present so reconnect can clear a stale wait"
        );
    }

    #[tokio::test]
    async fn concurrent_child_terminals_do_not_journal_a_stale_running_tree() {
        let (_dir, db) = store().await;
        let bus = CodeEventBus::default();
        let owner = OwnerId::local();
        let parent = session(&owner, SessionLifecycle::Idle);
        let mut first = session(&owner, SessionLifecycle::Running);
        let mut second = session(&owner, SessionLifecycle::Running);
        insert_session(&db, &parent).await.unwrap();
        insert_session(&db, &first).await.unwrap();
        insert_session(&db, &second).await.unwrap();
        set_session_context(&db, &owner, first.id, None, Some(parent.id), Some("first"))
            .await
            .unwrap();
        set_session_context(
            &db,
            &owner,
            second.id,
            None,
            Some(parent.id),
            Some("second"),
        )
        .await
        .unwrap();
        persist_session(&db, &bus, &first).await.unwrap();
        persist_session(&db, &bus, &second).await.unwrap();

        first.lifecycle = SessionLifecycle::Fenced;
        first.fence_reason = Some(FenceReason::OrphanAlive);
        second.lifecycle = SessionLifecycle::Ended;
        let first_persist = persist_session(&db, &bus, &first);
        let second_persist = persist_session(&db, &bus, &second);
        let (first_ok, second_ok) = tokio::join!(first_persist, second_persist);
        first_ok.unwrap();
        second_ok.unwrap();

        let expected = compute(&db, &owner, parent.id, None).await.unwrap();
        let page = tidebreak_core::db::code::list_events(&db, &owner, parent.id, 0, 50)
            .await
            .unwrap();
        let last = page
            .events
            .iter()
            .rev()
            .find_map(|entry| match &entry.event {
                Event::SessionTree { children, wait } => Some((children.clone(), wait.clone())),
                _ => None,
            })
            .expect("parent journal must carry a session_tree");
        assert_eq!(last, expected);
        assert!(
            last.0
                .iter()
                .all(|child| child.status != SessionTreeChildStatus::Running),
            "a slower running compute must not land after a terminal update: {last:?}"
        );
    }

    #[tokio::test]
    async fn named_wait_counts_only_those_children_and_clears_when_they_settle() {
        let (_dir, db) = store().await;
        let owner = OwnerId::local();
        let parent = session(&owner, SessionLifecycle::Idle);
        let mut waited = session(&owner, SessionLifecycle::Running);
        let extra = session(&owner, SessionLifecycle::Running);
        insert_session(&db, &parent).await.unwrap();
        insert_session(&db, &waited).await.unwrap();
        insert_session(&db, &extra).await.unwrap();
        set_session_context(
            &db,
            &owner,
            waited.id,
            None,
            Some(parent.id),
            Some("waited"),
        )
        .await
        .unwrap();
        set_session_context(&db, &owner, extra.id, None, Some(parent.id), Some("extra"))
            .await
            .unwrap();

        set_parent_wait_ids(&db, &owner, parent.id, &[waited.id])
            .await
            .unwrap();
        let (children, wait) = compute(&db, &owner, parent.id, None).await.unwrap();
        assert_eq!(children.len(), 2);
        assert_eq!(
            wait,
            Some(SessionTreeWait {
                waiting: 1,
                total: 1
            })
        );

        waited.lifecycle = SessionLifecycle::Ended;
        save_session(&db, &waited).await.unwrap();
        let (_, wait) = compute(&db, &owner, parent.id, None).await.unwrap();
        assert!(wait.is_none(), "settled named children must clear the wait");
        assert!(parent_wait_ids(&db, &owner, parent.id)
            .await
            .unwrap()
            .is_none());
    }
}
