//! The durable `code_wait` park: a parent turn that waits on child sessions
//! across a restart, and resumes once with their ordered results.

use super::{sample_chat, temp_store, test_checkpoint_progress};
use crate::{
    AcceptTurnOutcome, CallId, OwnerId, ParkTurnForClientCallOutcome, SessionId,
    SettleChildSessionWaitOutcome, Store, ToolCallExecution, ToolCallStatus, TurnId, TurnParkWait,
    TurnRunStatus, TurnStatus, CODE_WAIT_TOOL,
};
use chrono::Utc;

/// Park one claimed turn on `code_wait` and attach the code-session park the
/// adapter records beside it.
async fn park_on_children(
    store: &crate::DbStore,
    chat_id: SessionId,
    children: &[SessionId],
) -> (TurnId, CallId) {
    let turn_id = TurnId::new();
    assert!(matches!(
        store
            .accept_turn(turn_id, chat_id, "gpt-5", "ship both repositories")
            .await
            .unwrap(),
        AcceptTurnOutcome::Accepted(_)
    ));
    let claimed_at = Utc::now();
    let turn_lease = uuid::Uuid::new_v4();
    let claimed = store
        .claim_turn(
            turn_lease,
            claimed_at,
            claimed_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap()
        .turn
        .unwrap();
    assert_eq!(claimed.id, turn_id);
    let request = crate::model::ClientToolCallRequest {
        id: CallId::new(),
        chat_id,
        turn_id,
        provider_id: "native".into(),
        name: CODE_WAIT_TOOL.into(),
        arguments: serde_json::json!({"session_ids": children}),
    };
    assert!(matches!(
        store
            .park_turn_for_client_tool_call(
                turn_id,
                turn_lease,
                0,
                test_checkpoint_progress(),
                Utc::now(),
                &request,
            )
            .await
            .unwrap(),
        Some(ParkTurnForClientCallOutcome::Parked { .. })
    ));
    let park_ref = request.id.to_string();
    assert_eq!(
        crate::db::code::store_turn_park(
            store,
            &OwnerId::local(),
            crate::TurnId(turn_id.0),
            &park_ref,
            &TurnParkWait::ChildSessions {
                session_ids: children.iter().map(ToString::to_string).collect(),
            },
        )
        .await
        .unwrap(),
        Some(TurnStatus::Waiting)
    );
    (turn_id, request.id)
}

fn ordered_result(children: &[SessionId]) -> String {
    serde_json::json!({
        "waiting": false,
        "sessions": children
            .iter()
            .map(|id| serde_json::json!({"session_id": id, "running": false}))
            .collect::<Vec<_>>(),
    })
    .to_string()
}

/// The park outlives the process that wrote it. A fresh store over the same
/// database sees which children the parent named, settles the wait once the
/// children are done, and hands back one resumable turn whose result keeps
/// the order the parent asked for.
#[tokio::test]
async fn a_parked_parent_resumes_once_with_both_child_results_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}?mode=rwc", dir.path().join("test.db").display());
    let store = crate::DbStore::connect_test_sqlite_fixture_with_max_connections(&url, 1)
        .await
        .unwrap();
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let children = vec![SessionId::new(), SessionId::new()];
    let (turn_id, call_id) = park_on_children(&store, chat.id, &children).await;

    // Restart: drop every handle and reopen the same database, the way a
    // fresh worker comes up over the state the old one left behind.
    let database = url
        .strip_prefix("sqlite://")
        .and_then(|path| path.strip_suffix("?mode=rwc"))
        .unwrap()
        .to_owned();
    drop(store);
    let store = crate::DbStore::connect_test_sqlite(&format!("sqlite://{database}?mode=rw"), 1)
        .await
        .unwrap();

    let parked = crate::db::code::get_turn(&store, &OwnerId::local(), crate::TurnId(turn_id.0))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(parked.status, TurnStatus::Waiting);
    assert_eq!(
        parked.park_ref.as_deref(),
        Some(call_id.to_string().as_str())
    );
    assert_eq!(
        parked.park_wait,
        Some(TurnParkWait::ChildSessions {
            session_ids: children.iter().map(ToString::to_string).collect(),
        })
    );
    let pending = store
        .list_tool_calls(chat.id)
        .await
        .unwrap()
        .into_iter()
        .find(|call| call.id == call_id)
        .expect("the parked wait keeps its call");
    assert_eq!(pending.name, CODE_WAIT_TOOL);
    // Nobody leases this call: the server supplies its result.
    assert_eq!(pending.execution, ToolCallExecution::Orchestration);
    assert_eq!(pending.status, ToolCallStatus::Pending);
    assert_eq!(pending.result, None);

    let result = ordered_result(&children);
    let settled = store
        .settle_child_session_wait(chat.id, call_id, &result, Utc::now())
        .await
        .unwrap();
    let SettleChildSessionWaitOutcome::Settled { turn, .. } = settled else {
        panic!("unexpected settle outcome: {settled:?}")
    };
    assert_eq!(turn.status, TurnRunStatus::Resuming);

    // Once, not twice: an exact retry recovers the same commit and a
    // different result is refused rather than overwriting it.
    assert!(matches!(
        store
            .settle_child_session_wait(chat.id, call_id, &result, Utc::now())
            .await
            .unwrap(),
        SettleChildSessionWaitOutcome::Existing(_)
    ));
    assert!(matches!(
        store
            .settle_child_session_wait(chat.id, call_id, r#"{"waiting":false}"#, Utc::now())
            .await
            .unwrap(),
        SettleChildSessionWaitOutcome::ResultConflict
    ));

    let resumed = crate::db::code::get_turn(&store, &OwnerId::local(), crate::TurnId(turn_id.0))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resumed.status, TurnStatus::Resuming);
    let completed = store
        .list_tool_calls(chat.id)
        .await
        .unwrap()
        .into_iter()
        .find(|call| call.id == call_id)
        .expect("the settled wait keeps its call");
    assert_eq!(completed.status, ToolCallStatus::Completed);
    let stored: serde_json::Value = serde_json::from_str(completed.result.as_deref().unwrap())
        .expect("the stored result is the tool's own shape");
    assert_eq!(stored["waiting"], false);
    assert_eq!(
        stored["sessions"][0]["session_id"],
        serde_json::json!(children[0])
    );
    assert_eq!(
        stored["sessions"][1]["session_id"],
        serde_json::json!(children[1])
    );

    // The adapter clears the exact park it wrote before it resumes the leg.
    assert_eq!(
        crate::db::code::clear_turn_park(
            &store,
            &OwnerId::local(),
            crate::TurnId(turn_id.0),
            &call_id.to_string(),
            &TurnParkWait::ChildSessions {
                session_ids: children.iter().map(ToString::to_string).collect(),
            },
        )
        .await
        .unwrap(),
        Some(TurnStatus::Resuming)
    );
}

/// Cancelling the parent closes the wait rather than leaving a turn parked on
/// children nobody will read.
#[tokio::test]
async fn cancelling_the_parent_closes_its_child_wait() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let children = vec![SessionId::new()];
    let (turn_id, call_id) = park_on_children(&store, chat.id, &children).await;

    store
        .request_turn_cancellation_and_append_event(turn_id, Utc::now())
        .await
        .unwrap()
        .expect("a parked parent takes a cancellation");

    let settled = store
        .settle_child_session_wait(chat.id, call_id, &ordered_result(&children), Utc::now())
        .await
        .unwrap();
    assert!(
        matches!(settled, SettleChildSessionWaitOutcome::Unavailable),
        "a cancelled wait cannot be settled: {settled:?}"
    );
}
