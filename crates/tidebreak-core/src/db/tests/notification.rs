use super::*;
use crate::storage::{
    code_session_mints_notification, notification_body_line, notification_kind_for_turn_status,
    NotificationContext, NotificationKind, MAX_NOTIFICATION_BODY_CHARS,
};
use crate::SessionKind;
use crate::TurnRunStatus;

#[test]
fn a_body_is_the_first_line_with_words_in_plain_text() {
    assert_eq!(
        notification_body_line("\n\n## Fixed the **login** check\n\nDetails.").as_deref(),
        Some("Fixed the login check")
    );
    assert_eq!(
        notification_body_line("```rust\nfn main() {}\n```").as_deref(),
        Some("fn main() {}")
    );
    assert_eq!(
        notification_body_line("- Ran `cargo test`").as_deref(),
        Some("Ran cargo test")
    );
    assert_eq!(
        notification_body_line("1. Read the tree").as_deref(),
        Some("Read the tree")
    );
    // A citation directive is the renderer's markup. The banner keeps only
    // the words it wraps.
    assert_eq!(
        notification_body_line(
            "Revenue grew :cit[12% last year]{doc=018f4f8f-6c6e-7f80-8000-000000000001 page=3}."
        )
        .as_deref(),
        Some("Revenue grew 12% last year.")
    );
    // A heading marker needs its space; a hash in prose stays.
    assert_eq!(
        notification_body_line("#3521 is merged").as_deref(),
        Some("#3521 is merged")
    );
    // A direction override would show the banner's words out of order.
    assert_eq!(
        notification_body_line("Removed \u{202E}sgol\u{202C} and\u{200B} caches").as_deref(),
        Some("Removed sgol and caches")
    );
    assert_eq!(notification_body_line("  \n```\n```\n"), None);
}

#[test]
fn a_long_body_is_cut_to_one_banner() {
    let body = notification_body_line(&"word ".repeat(100)).unwrap();
    assert_eq!(body.chars().count(), MAX_NOTIFICATION_BODY_CHARS);
    assert!(body.ends_with('…'));
}

#[test]
fn only_a_provider_failure_says_why() {
    assert_eq!(
        crate::provider_failure_detail("rate_limited", " Too many requests. "),
        Some("Too many requests.".to_owned())
    );
    // Internal kinds can carry host paths.
    assert_eq!(
        crate::provider_failure_detail("store", "/Users/me/Library failed"),
        None
    );
    assert_eq!(crate::provider_failure_detail("provider", "  "), None);
}

#[tokio::test]
async fn a_finished_work_turn_shows_its_closing_message() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn_id = crate::TurnId::new();
    for (text, role) in [
        ("What changed?", "user"),
        ("Three files changed.\nHere is the list.", "assistant"),
    ] {
        let seq = super::ops::conversation::next_message_seq_on(&store.conn, chat.id)
            .await
            .unwrap();
        entities::message::ActiveModel {
            id: Set(MessageId::new().0),
            chat_id: Set(chat.id.0),
            turn_id: Set(turn_id.0),
            seq: Set(seq),
            role: Set(role.into()),
            reasoning: Default::default(),
            content: Set(text.into()),
            llm_content: Set(None),
            turn_lease_token: Set(None),
            created_at: Set(Utc::now()),
        }
        .insert(&store.conn)
        .await
        .unwrap();
    }

    let row = store
        .record_work_turn_notification(chat.id, turn_id, NotificationKind::AgentCompleted)
        .await
        .unwrap()
        .expect("the settle writes a row");

    assert_eq!(row.body.as_deref(), Some("Three files changed."));
}

#[tokio::test]
async fn a_failed_work_turn_says_why_only_when_the_provider_did() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let provider = crate::AgentErrorInfo {
        kind: "rate_limited".into(),
        message: "The provider is rate-limiting requests.".into(),
    };
    let internal = crate::AgentErrorInfo {
        kind: "store".into(),
        message: "database is locked at /Users/me/profile.db".into(),
    };

    let mut bodies = Vec::new();
    for failure in [&provider, &internal] {
        let row = super::ops::notification::record_work_turn_notification_on(
            &store.conn,
            chat.id,
            crate::TurnId::new(),
            NotificationKind::AgentFailed,
            Some(failure),
        )
        .await
        .unwrap()
        .expect("the settle writes a row");
        bodies.push(row.body);
    }

    assert_eq!(
        bodies,
        [
            Some("The provider is rate-limiting requests.".to_owned()),
            None
        ]
    );
}

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

    let first = crate::db::ops::notification::record_code_turn_notification(
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
    let second = crate::db::ops::notification::record_code_turn_notification(
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
