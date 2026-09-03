use super::*;
use crate::storage::{
    code_session_mints_notification, notification_kind_for_turn_status, NotificationContext,
    NotificationKind,
};
use crate::SessionKind;
use crate::TurnRunStatus;

#[test]
fn cancel_and_retry_do_not_mint_a_notification() {
    assert_eq!(
        notification_kind_for_turn_status(TurnRunStatus::Cancelled),
        None
    );
    assert_eq!(
        notification_kind_for_turn_status(TurnRunStatus::RetryWait),
        None
    );
    assert_eq!(
        notification_kind_for_turn_status(TurnRunStatus::Completed),
        Some(NotificationKind::AgentCompleted)
    );
    assert_eq!(
        notification_kind_for_turn_status(TurnRunStatus::Failed),
        Some(NotificationKind::AgentFailed)
    );
}

#[test]
fn a_watch_session_does_not_mint_a_notification() {
    assert!(!code_session_mints_notification(SessionKind::Watch));
    assert!(code_session_mints_notification(SessionKind::Interactive));
}

#[tokio::test]
async fn a_work_turn_cannot_double_insert() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn_id = crate::TurnId::new();

    let first = store
        .record_work_turn_notification(chat.id, turn_id, NotificationKind::AgentCompleted)
        .await
        .unwrap()
        .expect("the first settle writes a row");
    let second = store
        .record_work_turn_notification(chat.id, turn_id, NotificationKind::AgentCompleted)
        .await
        .unwrap()
        .expect("a reconnect returns the same row");
    assert_eq!(first.id, second.id);

    let owner = crate::OwnerId::local();
    let page = store
        .list_notifications_scoped(&owner, None, 50)
        .await
        .unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].title, "hello finished");
    assert_eq!(
        page[0].context,
        NotificationContext::Chat { chat_id: chat.id }
    );
    assert_eq!(
        store
            .unread_notification_count_scoped(&owner)
            .await
            .unwrap(),
        1
    );

    store
        .mark_notifications_read_scoped(&owner, &[first.id], chrono::Utc::now())
        .await
        .unwrap();
    assert_eq!(
        store
            .unread_notification_count_scoped(&owner)
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn a_code_turn_cannot_double_insert() {
    let (_dir, store) = temp_store().await;
    let owner = crate::OwnerId::local();
    let session_id = crate::SessionId::new();
    let workspace_id = crate::WorkspaceId::new();
    let turn_id = crate::TurnId::new();

    let first = crate::db::code::record_code_turn_notification(
        &store,
        &owner,
        session_id,
        workspace_id,
        turn_id,
        Some("first"),
        NotificationKind::AgentFailed,
    )
    .await
    .unwrap();
    let second = crate::db::code::record_code_turn_notification(
        &store,
        &owner,
        session_id,
        workspace_id,
        turn_id,
        Some("first"),
        NotificationKind::AgentFailed,
    )
    .await
    .unwrap();
    assert_eq!(first.id, second.id);
    assert_eq!(first.title, "first failed");

    let page = store
        .list_notifications_scoped(&owner, None, 50)
        .await
        .unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!(
        page[0].context,
        NotificationContext::Code {
            session_id,
            workspace_id,
        }
    );
}
