use super::{entities, sample_chat, temp_store};
use crate::event::AgentEvent;
use crate::model::{Message, Role, TurnRunStatus};
use crate::provider::{RefusalDetails, RefusalOutcome, StopReason, Usage};
use crate::storage::{AcceptTurnOutcome, CompleteTurnRunOutcome, NotificationKind, Store};
use crate::{Chat, DbStore, MessageId, OwnerId, TurnId};
use chrono::Duration;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

async fn claimed_turn(
    store: &DbStore,
    prompt: &str,
) -> (Chat, TurnId, uuid::Uuid, chrono::DateTime<chrono::Utc>) {
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    let turn = match store
        .accept_turn(turn_id, chat.id, "test-model", prompt)
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    let claimed_at = turn.available_at + Duration::seconds(1);
    let lease_token = uuid::Uuid::new_v4();
    store
        .claim_turn(lease_token, claimed_at, claimed_at + Duration::minutes(1))
        .await
        .unwrap()
        .turn
        .expect("accepted turn is claimable");
    (chat, turn_id, lease_token, claimed_at)
}

async fn set_checkpoint(store: &DbStore, turn_id: TurnId, model_steps: i32, usage: Usage) {
    let updated = entities::code_turn::Entity::update_many()
        .col_expr(
            entities::code_turn::Column::ModelSteps,
            sea_orm::sea_query::Expr::value(model_steps),
        )
        .col_expr(
            entities::code_turn::Column::InputTokens,
            sea_orm::sea_query::Expr::value(i64::from(usage.input_tokens)),
        )
        .col_expr(
            entities::code_turn::Column::OutputTokens,
            sea_orm::sea_query::Expr::value(i64::from(usage.output_tokens)),
        )
        .col_expr(
            entities::code_turn::Column::CacheReadInputTokens,
            sea_orm::sea_query::Expr::value(i64::from(usage.cache_read_input_tokens)),
        )
        .col_expr(
            entities::code_turn::Column::CacheCreationInputTokens,
            sea_orm::sea_query::Expr::value(i64::from(usage.cache_creation_input_tokens)),
        )
        .filter(entities::code_turn::Column::Id.eq(turn_id.0))
        .filter(entities::code_turn::Column::Status.eq(TurnRunStatus::Running.as_str()))
        .exec(&store.conn)
        .await
        .unwrap();
    assert_eq!(updated.rows_affected, 1);
}

#[tokio::test]
async fn completed_terminal_event_persists_authoritative_totals_without_checkpoint() {
    let (_dir, store) = temp_store().await;
    let (chat, turn_id, lease_token, claimed_at) =
        claimed_turn(&store, "persist completion totals").await;
    let usage = Usage {
        input_tokens: 31,
        output_tokens: 17,
        cache_read_input_tokens: 11,
        cache_creation_input_tokens: 7,
    };
    let output = Message {
        id: MessageId::new(),
        chat_id: chat.id,
        turn_id,
        role: Role::Assistant,
        reasoning: Default::default(),
        content: "done".into(),
        llm_content: None,
        created_at: claimed_at + Duration::seconds(1),
    };

    let completed = super::super::ops::turn::complete_turn_and_append_event(
        &store,
        turn_id,
        lease_token,
        0,
        output.created_at,
        &output,
        3,
        usage,
        StopReason::EndTurn,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(
        completed.outcome,
        CompleteTurnRunOutcome::Completed(ref turn)
            if turn.model_steps == 3 && turn.usage == usage
    ));
    assert!(matches!(
        completed.terminal_event.as_ref().map(|event| &event.event),
        Some(AgentEvent::TurnCompleted { usage: stored, .. }) if *stored == usage
    ));
    let stored = store.get_turn(turn_id).await.unwrap().unwrap();
    assert_eq!((stored.model_steps, stored.usage), (3, usage));
    let notifications = store
        .list_notifications_scoped(&OwnerId::local(), None, 50)
        .await
        .unwrap();
    assert_eq!(notifications.len(), 1);
    assert_eq!(notifications[0].kind, NotificationKind::AgentCompleted);
    assert_eq!(notifications[0].title, "hello finished");

    let replay = super::super::ops::turn::complete_turn_and_append_event(
        &store,
        turn_id,
        lease_token,
        0,
        output.created_at + Duration::seconds(1),
        &output,
        3,
        usage,
        StopReason::EndTurn,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(
        replay.outcome,
        CompleteTurnRunOutcome::Existing(ref turn)
            if turn.model_steps == 3 && turn.usage == usage
    ));
    assert_eq!(replay.terminal_event, completed.terminal_event);
    assert_eq!(
        store
            .list_notifications_scoped(&OwnerId::local(), None, 50)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(super::super::ops::turn::complete_turn_and_append_event(
        &store,
        turn_id,
        lease_token,
        0,
        output.created_at + Duration::seconds(2),
        &output,
        4,
        usage,
        StopReason::EndTurn,
    )
    .await
    .is_err());
}

#[tokio::test]
async fn refused_terminal_event_replaces_checkpoint_with_authoritative_totals() {
    let (_dir, store) = temp_store().await;
    let (chat, turn_id, lease_token, claimed_at) =
        claimed_turn(&store, "persist refusal totals").await;
    let checkpoint = Usage {
        input_tokens: 13,
        output_tokens: 5,
        cache_read_input_tokens: 3,
        cache_creation_input_tokens: 2,
    };
    set_checkpoint(&store, turn_id, 1, checkpoint).await;
    let total = Usage {
        input_tokens: 29,
        output_tokens: 12,
        cache_read_input_tokens: 8,
        cache_creation_input_tokens: 6,
    };
    let output = Message {
        id: MessageId::new(),
        chat_id: chat.id,
        turn_id,
        role: Role::Assistant,
        reasoning: Default::default(),
        content: "partial answer".into(),
        llm_content: None,
        created_at: claimed_at + Duration::seconds(1),
    };
    let refusal = RefusalOutcome::new(RefusalDetails::from_category(Some("policy")), true);

    let completed = super::super::ops::turn::complete_refused_turn_with_citations_and_append_event(
        &store,
        turn_id,
        lease_token,
        0,
        output.created_at,
        &output,
        &[],
        4,
        total,
        refusal.clone(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(
        completed.outcome,
        CompleteTurnRunOutcome::Completed(ref turn)
            if turn.model_steps == 4 && turn.usage == total
    ));
    assert!(matches!(
        completed.terminal_event.as_ref().map(|event| &event.event),
        Some(AgentEvent::TurnRefused { usage, refusal: stored })
            if *usage == total && stored == &refusal
    ));
    let stored = store.get_turn(turn_id).await.unwrap().unwrap();
    assert_eq!((stored.model_steps, stored.usage), (4, total));
    assert_ne!(stored.usage, checkpoint);
    let notifications = store
        .list_notifications_scoped(&OwnerId::local(), None, 50)
        .await
        .unwrap();
    assert_eq!(notifications.len(), 1);
    assert_eq!(notifications[0].kind, NotificationKind::AgentCompleted);
}

#[tokio::test]
async fn report_blocked_refusal_mints_a_failed_notification() {
    let (_dir, store) = temp_store().await;
    let (chat, turn_id, lease_token, claimed_at) =
        claimed_turn(&store, "report an operational blocker").await;
    let output = Message {
        id: MessageId::new(),
        chat_id: chat.id,
        turn_id,
        role: Role::Assistant,
        reasoning: Default::default(),
        content: "Docker is unavailable, so I could not run the required script.".into(),
        llm_content: None,
        created_at: claimed_at + Duration::seconds(1),
    };

    super::super::ops::turn::complete_refused_turn_with_citations_and_append_event(
        &store,
        turn_id,
        lease_token,
        0,
        output.created_at,
        &output,
        &[],
        1,
        Usage::default(),
        RefusalOutcome::report_blocked(),
    )
    .await
    .unwrap()
    .expect("live blocked completion");

    let notifications = store
        .list_notifications_scoped(&OwnerId::local(), None, 50)
        .await
        .unwrap();
    assert_eq!(notifications.len(), 1);
    assert_eq!(notifications[0].kind, NotificationKind::AgentFailed);
    assert_eq!(notifications[0].title, "hello failed");
}

#[tokio::test]
async fn provider_blocked_refusal_remains_a_completed_notification() {
    let (_dir, store) = temp_store().await;
    let (chat, turn_id, lease_token, claimed_at) =
        claimed_turn(&store, "persist a provider refusal").await;
    let output = Message {
        id: MessageId::new(),
        chat_id: chat.id,
        turn_id,
        role: Role::Assistant,
        reasoning: Default::default(),
        content: "Provider refusal".into(),
        llm_content: None,
        created_at: claimed_at + Duration::seconds(1),
    };

    super::super::ops::turn::complete_refused_turn_with_citations_and_append_event(
        &store,
        turn_id,
        lease_token,
        0,
        output.created_at,
        &output,
        &[],
        1,
        Usage::default(),
        RefusalOutcome::new(RefusalDetails::from_category(Some("blocked")), true),
    )
    .await
    .unwrap()
    .expect("live provider refusal completion");

    let notifications = store
        .list_notifications_scoped(&OwnerId::local(), None, 50)
        .await
        .unwrap();
    assert_eq!(notifications.len(), 1);
    assert_eq!(notifications[0].kind, NotificationKind::AgentCompleted);
    assert_eq!(notifications[0].title, "hello finished");
}
