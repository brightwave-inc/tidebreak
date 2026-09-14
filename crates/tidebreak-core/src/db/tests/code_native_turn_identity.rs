use super::*;
use crate::code::{CodeIncarnationId, IncarnationAdmission};
use crate::db::code::*;

async fn incarnation(store: &crate::DbStore, session: SessionId) -> CodeIncarnationId {
    let mut row = get_session(store, &OwnerId::local(), session)
        .await
        .unwrap()
        .unwrap();
    row.lifecycle = SessionLifecycle::Running;
    save_session(store, &row).await.unwrap();
    let IncarnationAdmission::Admitted(row) =
        create_incarnation_intent(store, &OwnerId::local(), session, 1, 10)
            .await
            .unwrap()
    else {
        panic!("incarnation admitted")
    };
    activate_incarnation(store, &OwnerId::local(), row.id, "test-sandbox")
        .await
        .unwrap();
    row.id
}

#[tokio::test]
async fn native_identity_completion_before_receipt_replays_once_after_binding() {
    let (dir, store, session, turn) = seeded_session().await;
    let owner = OwnerId::local();
    let mut row = get_session(&store, &owner, session).await.unwrap().unwrap();
    row.lifecycle = SessionLifecycle::Running;
    save_session(&store, &row).await.unwrap();
    let url = format!("sqlite://{}?mode=rwc", dir.path().join("test.db").display());
    let inc = incarnation(&store, session).await;
    let runtime = uuid::Uuid::new_v4();
    assert_eq!(
        observe_native_turn(&store, &owner, session, inc, runtime, 3, "inbox", &[17])
            .await
            .unwrap(),
        None
    );
    record_native_turn_output(
        &store,
        &owner,
        session,
        inc,
        runtime,
        3,
        &serde_json::json!({"body":"Saved the fix"}),
    )
    .await
    .unwrap();
    record_native_turn_terminal(
        &store,
        &owner,
        session,
        inc,
        runtime,
        3,
        TurnStatus::Completed,
    )
    .await
    .unwrap();
    assert!(
        !project_native_turn(&store, &owner, session, inc, runtime, turn)
            .await
            .unwrap()
            .1
    );
    drop(store);
    let store = crate::DbStore::connect(&url).await.unwrap();
    record_native_turn_input(&store, &owner, session, inc, turn, Some(17))
        .await
        .unwrap();
    record_native_turn_input(&store, &owner, session, inc, turn, Some(17))
        .await
        .unwrap();
    assert_eq!(
        native_turn_for_host(&store, &owner, session, inc, runtime, turn)
            .await
            .unwrap(),
        Some(3)
    );
    let (events, settled) = project_native_turn(&store, &owner, session, inc, runtime, turn)
        .await
        .unwrap();
    assert!(settled);
    assert_eq!(events.len(), 3);
    assert!(matches!(events[0].event,Event::TurnStarted{turn_id} if turn_id==turn));
    assert!(matches!(&events[1].event,Event::AssistantMessage{text,..} if text=="Saved the fix"));
    assert!(matches!(events[2].event, Event::TurnCompleted { .. }));
    drop(store);
    let store = crate::DbStore::connect(&url).await.unwrap();
    assert_eq!(
        get_session(&store, &owner, session)
            .await
            .unwrap()
            .unwrap()
            .lifecycle,
        SessionLifecycle::Idle
    );
    assert_eq!(
        get_turn(&store, &owner, turn)
            .await
            .unwrap()
            .unwrap()
            .status,
        TurnStatus::Completed
    );
    assert_eq!(
        project_native_turn(&store, &owner, session, inc, runtime, turn)
            .await
            .unwrap(),
        (vec![], false)
    );
    assert_eq!(
        observe_native_turn(&store, &owner, session, inc, runtime, 3, "inbox", &[17])
            .await
            .unwrap(),
        Some(turn)
    );
}

#[tokio::test]
async fn native_identity_control_and_goal_turns_never_bind_the_next_user_turn() {
    let (_dir, store, session, turn) = seeded_session().await;
    let owner = OwnerId::local();
    let inc = incarnation(&store, session).await;
    let runtime = uuid::Uuid::new_v4();
    observe_native_turn(&store, &owner, session, inc, runtime, 2, "inbox", &[11])
        .await
        .unwrap();
    observe_native_turn(&store, &owner, session, inc, runtime, 3, "goal_resume", &[])
        .await
        .unwrap();
    record_native_turn_terminal(
        &store,
        &owner,
        session,
        inc,
        runtime,
        2,
        TurnStatus::Completed,
    )
    .await
    .unwrap();
    record_native_turn_input(&store, &owner, session, inc, turn, Some(12))
        .await
        .unwrap();
    assert_eq!(
        observe_native_turn(&store, &owner, session, inc, runtime, 4, "inbox", &[12])
            .await
            .unwrap(),
        Some(turn)
    );
    for counter in [2, 3] {
        assert_eq!(
            hosted_turn_for_native(&store, &owner, session, inc, runtime, counter)
                .await
                .unwrap(),
            None
        );
    }
    let (events, settled) = project_native_turn(&store, &owner, session, inc, runtime, turn)
        .await
        .unwrap();
    assert!(!settled);
    assert_eq!(events.len(), 1);
    assert_eq!(
        get_turn(&store, &owner, turn)
            .await
            .unwrap()
            .unwrap()
            .status,
        TurnStatus::Running
    );
}

#[tokio::test]
async fn native_identity_rejects_changed_replay_and_cross_owner_access() {
    let (_dir, store, session, turn) = seeded_session().await;
    let owner = OwnerId::local();
    let inc = incarnation(&store, session).await;
    let runtime = uuid::Uuid::new_v4();
    record_native_turn_input(&store, &owner, session, inc, turn, Some(1))
        .await
        .unwrap();
    observe_native_turn(&store, &owner, session, inc, runtime, 1, "inbox", &[1])
        .await
        .unwrap();
    assert!(
        record_native_turn_input(&store, &owner, session, inc, turn, Some(2))
            .await
            .is_err()
    );
    assert!(
        observe_native_turn(&store, &owner, session, inc, runtime, 1, "inbox", &[2])
            .await
            .is_err()
    );
    assert!(
        observe_native_turn(&store, &owner, session, inc, runtime, 2, "inbox", &[1])
            .await
            .is_err()
    );
    let other = OwnerId::new("other").unwrap();
    assert!(
        native_turn_for_host(&store, &other, session, inc, runtime, turn)
            .await
            .is_err()
    );
    assert!(
        record_native_turn_input(&store, &other, session, inc, turn, Some(1))
            .await
            .is_err()
    );
    assert!(
        observe_native_turn(&store, &other, session, inc, runtime, 4, "inbox", &[4])
            .await
            .is_err()
    );
    record_native_turn_terminal(
        &store,
        &owner,
        session,
        inc,
        runtime,
        1,
        TurnStatus::Completed,
    )
    .await
    .unwrap();
    assert!(record_native_turn_terminal(
        &store,
        &owner,
        session,
        inc,
        runtime,
        1,
        TurnStatus::Interrupted
    )
    .await
    .is_err());
}

#[tokio::test]
async fn native_identity_turn_insertion_and_receipt_fail_atomically() {
    let (_dir, store, session, turn) = seeded_session().await;
    let owner = OwnerId::local();
    let inc = incarnation(&store, session).await;
    record_native_turn_input(&store, &owner, session, inc, turn, Some(17))
        .await
        .unwrap();
    let mut next = get_turn(&store, &owner, turn).await.unwrap().unwrap();
    next.id = TurnId::new();
    next.ordinal = 2;
    assert!(
        insert_remote_turn_with_input(&store, &owner, inc, &next, None, Some(17))
            .await
            .is_err()
    );
    assert!(
        get_turn(&store, &owner, next.id).await.unwrap().is_none(),
        "failed receipt rolls back the running turn"
    );
    let next = insert_remote_turn_with_input(&store, &owner, inc, &next, None, Some(18))
        .await
        .unwrap();
    let runtime = uuid::Uuid::new_v4();
    assert_eq!(
        observe_native_turn(&store, &owner, session, inc, runtime, 3, "inbox", &[18])
            .await
            .unwrap(),
        Some(next.id)
    );
}

#[tokio::test]
async fn native_identity_completion_preserves_a_fenced_session() {
    let (_dir, store, session, turn) = seeded_session().await;
    let owner = OwnerId::local();
    let inc = incarnation(&store, session).await;
    let runtime = uuid::Uuid::new_v4();
    record_native_turn_input(&store, &owner, session, inc, turn, None)
        .await
        .unwrap();
    observe_native_turn(&store, &owner, session, inc, runtime, 1, "spawn_task", &[])
        .await
        .unwrap();
    record_native_turn_terminal(
        &store,
        &owner,
        session,
        inc,
        runtime,
        1,
        TurnStatus::Completed,
    )
    .await
    .unwrap();
    let mut row = get_session(&store, &owner, session).await.unwrap().unwrap();
    row.lifecycle = SessionLifecycle::Fenced;
    save_session(&store, &row).await.unwrap();
    let before = list_events(&store, &owner, session, 0, 500)
        .await
        .unwrap()
        .events
        .len();
    assert_eq!(
        project_native_turn(&store, &owner, session, inc, runtime, turn)
            .await
            .unwrap(),
        (vec![], false)
    );
    assert_eq!(
        get_session(&store, &owner, session)
            .await
            .unwrap()
            .unwrap()
            .lifecycle,
        SessionLifecycle::Fenced
    );
    assert_eq!(
        list_events(&store, &owner, session, 0, 500)
            .await
            .unwrap()
            .events
            .len(),
        before
    );
    assert_eq!(
        get_turn(&store, &owner, turn)
            .await
            .unwrap()
            .unwrap()
            .status,
        TurnStatus::Running
    );
}
