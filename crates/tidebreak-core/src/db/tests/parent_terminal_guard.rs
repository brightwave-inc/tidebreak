use super::{sample_chat, temp_store};
use crate::db::tests::agent_run::{admit_sandbox_call_for_test, live_turn_for_sandbox_test};
use crate::{
    AgentRunCancellationReason, AgentRunCheckInReason, AgentRunId, AgentRunInboxStatus,
    AgentRunStatus, CallId, CompleteTurnRunOutcome, Message, MessageId, RecordTurnFailureOutcome,
    RequestAgentRunCancellationOutcome, Role, Store, SubmitAgentRunResultOutcome, TurnFailureRetry,
    TurnRunStatus, Usage,
};
use chrono::{Duration, Utc};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

async fn complete_child(store: &crate::DbStore, chat_id: crate::ChatId, task: &str) -> AgentRunId {
    let child = admit_sandbox_call_for_test(store, chat_id, CallId::new(), task).await;
    let lease = uuid::Uuid::new_v4();
    let claimed = store
        .claim_agent_run(lease, Duration::minutes(5), 1, 1)
        .await
        .unwrap()
        .expect("child should claim");
    assert_eq!(claimed.id, child.id);
    assert!(matches!(
        store
            .submit_agent_run_result(child.id, lease, "finished")
            .await
            .unwrap(),
        Some(SubmitAgentRunResultOutcome::Completed(_))
    ));
    child.id
}

async fn consume_child(store: &crate::DbStore, chat_id: crate::ChatId, child: AgentRunId) {
    let parent = AgentRunId::foreground_for_chat(chat_id);
    let lease = uuid::Uuid::new_v4();
    let delivered = store
        .list_agent_run_inbox(parent)
        .await
        .unwrap()
        .into_iter()
        .find(|entry| entry.child_run_id == child)
        .expect("terminal inbox should exist")
        .delivered_at;
    let updated = crate::db::entities::agent_run_inbox::Entity::update_many()
        .col_expr(
            crate::db::entities::agent_run_inbox::Column::Status,
            sea_orm::sea_query::Expr::value(AgentRunInboxStatus::Consumed.as_str()),
        )
        .col_expr(
            crate::db::entities::agent_run_inbox::Column::ClaimCount,
            sea_orm::sea_query::Expr::value(1),
        )
        .col_expr(
            crate::db::entities::agent_run_inbox::Column::ConsumedLeaseToken,
            sea_orm::sea_query::Expr::value(Some(lease)),
        )
        .col_expr(
            crate::db::entities::agent_run_inbox::Column::ConsumedAt,
            sea_orm::sea_query::Expr::value(Some(delivered)),
        )
        .filter(crate::db::entities::agent_run_inbox::Column::ChildRunId.eq(child.0))
        .exec(&store.conn)
        .await
        .unwrap();
    assert_eq!(updated.rows_affected, 1);
}

fn output_for(turn: &crate::TurnRun, chat_id: crate::ChatId) -> Message {
    Message {
        id: MessageId::new(),
        chat_id,
        turn_id: turn.id,
        role: Role::Assistant,
        reasoning: Default::default(),
        content: "foreground answer".into(),
        llm_content: None,
        created_at: Utc::now().max(turn.updated_at),
    }
}

#[tokio::test]
async fn completion_fences_children_in_stable_order_without_writing_output() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (turn, lease) = live_turn_for_sandbox_test(&store, chat.id).await;
    let first = admit_sandbox_call_for_test(&store, chat.id, CallId::new(), "first").await;
    let second = admit_sandbox_call_for_test(&store, chat.id, CallId::new(), "second").await;
    let output = output_for(&turn, chat.id);

    let fenced = store
        .complete_turn(turn.id, lease, 0, Utc::now(), &output)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        fenced,
        CompleteTurnRunOutcome::ChildrenOutstanding { child_run_ids, .. }
            if child_run_ids == vec![first.id, second.id]
    ));
    assert_eq!(
        store.get_turn(turn.id).await.unwrap().unwrap().status,
        TurnRunStatus::Running
    );
    assert!(!store
        .list_messages(chat.id)
        .await
        .unwrap()
        .iter()
        .any(|message| message.id == output.id));
    assert!(store.list_events(chat.id, 0).await.unwrap().is_empty());

    let first_lease = uuid::Uuid::new_v4();
    assert_eq!(
        store
            .claim_agent_run(first_lease, Duration::minutes(5), 1, 1)
            .await
            .unwrap()
            .unwrap()
            .id,
        first.id
    );
    store
        .submit_agent_run_result(first.id, first_lease, "first result")
        .await
        .unwrap()
        .unwrap();
    let still_fenced = store
        .complete_turn(turn.id, lease, 0, Utc::now(), &output)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        still_fenced,
        CompleteTurnRunOutcome::ChildrenOutstanding { child_run_ids, .. }
            if child_run_ids == vec![first.id, second.id]
    ));
    consume_child(&store, chat.id, first.id).await;

    assert!(matches!(
        store
            .request_agent_run_cancellation(second.id)
            .await
            .unwrap(),
        Some(RequestAgentRunCancellationOutcome::Cancelled(_))
    ));
    assert!(matches!(
        store
            .complete_turn(turn.id, lease, 0, Utc::now(), &output)
            .await
            .unwrap(),
        Some(CompleteTurnRunOutcome::ChildrenOutstanding { child_run_ids, .. })
            if child_run_ids == vec![second.id]
    ));
    consume_child(&store, chat.id, second.id).await;
    assert!(matches!(
        store
            .complete_turn(turn.id, lease, 0, Utc::now(), &output)
            .await
            .unwrap(),
        Some(CompleteTurnRunOutcome::Completed(_))
    ));
}

#[tokio::test]
async fn completion_fences_needs_input_checkin_without_store_error() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (turn, lease) = live_turn_for_sandbox_test(&store, chat.id).await;
    let child = admit_sandbox_call_for_test(&store, chat.id, CallId::new(), "check in").await;
    let child_lease = uuid::Uuid::new_v4();
    assert_eq!(
        store
            .claim_agent_run(child_lease, Duration::minutes(5), 1, 1)
            .await
            .unwrap()
            .unwrap()
            .id,
        child.id
    );
    assert!(matches!(
        store
            .submit_agent_run_checkin(
                child.id,
                child_lease,
                AgentRunCheckInReason::ConsecutiveToolErrors,
                3,
                "tool errors need direction",
            )
            .await
            .unwrap(),
        Some(SubmitAgentRunResultOutcome::Completed(_))
    ));
    assert_eq!(
        store.get_agent_run(child.id).await.unwrap().unwrap().status,
        AgentRunStatus::NeedsInput
    );
    let inbox = store
        .list_agent_run_inbox(AgentRunId::foreground_for_chat(chat.id))
        .await
        .unwrap();
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].child_run_id, child.id);
    assert_eq!(inbox[0].status, AgentRunInboxStatus::Pending);
    assert!(matches!(
        inbox[0].result.payload,
        crate::AgentRunResultPayload::CheckIn {
            reason: AgentRunCheckInReason::ConsecutiveToolErrors,
            ..
        }
    ));

    let output = output_for(&turn, chat.id);
    // The check-in delivery must fence completion as ChildrenOutstanding, not
    // as a store error the turn worker would retry forever.
    assert!(matches!(
        store
            .complete_turn(turn.id, lease, 0, Utc::now(), &output)
            .await
            .unwrap(),
        Some(CompleteTurnRunOutcome::ChildrenOutstanding { child_run_ids, .. })
            if child_run_ids == vec![child.id]
    ));
    assert_eq!(
        store.get_turn(turn.id).await.unwrap().unwrap().status,
        TurnRunStatus::Running
    );
    assert!(!store
        .list_messages(chat.id)
        .await
        .unwrap()
        .iter()
        .any(|message| message.id == output.id));

    // Consuming the check-in does not free the child: it is still paused, so
    // the parent remains fenced until resume or cancellation settles it.
    consume_child(&store, chat.id, child.id).await;
    assert!(matches!(
        store
            .complete_turn(turn.id, lease, 0, Utc::now(), &output)
            .await
            .unwrap(),
        Some(CompleteTurnRunOutcome::ChildrenOutstanding { child_run_ids, .. })
            if child_run_ids == vec![child.id]
    ));
}

#[tokio::test]
async fn permanent_failure_fences_live_children_and_retires_only_unconsumed_deliveries() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (turn, turn_lease) = live_turn_for_sandbox_test(&store, chat.id).await;
    let pending = complete_child(&store, chat.id, "pending result").await;
    let consumed = complete_child(&store, chat.id, "consumed result").await;
    consume_child(&store, chat.id, consumed).await;
    let running = admit_sandbox_call_for_test(&store, chat.id, CallId::new(), "running").await;
    let running_lease = uuid::Uuid::new_v4();
    assert_eq!(
        store
            .claim_agent_run(running_lease, Duration::minutes(5), 1, 1)
            .await
            .unwrap()
            .unwrap()
            .id,
        running.id
    );
    let queued = admit_sandbox_call_for_test(&store, chat.id, CallId::new(), "queued").await;

    let failure = store
        .record_turn_failure(
            turn.id,
            turn_lease,
            Utc::now(),
            TurnFailureRetry::Permanent,
            0,
            Usage::default(),
            "provider_failed",
            Some("provider failed permanently"),
        )
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(failure, RecordTurnFailureOutcome::Recorded(_)));
    assert_eq!(
        store.get_turn(turn.id).await.unwrap().unwrap().status,
        TurnRunStatus::Failed
    );
    assert_eq!(
        store
            .get_agent_run(running.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        AgentRunStatus::Cancelling
    );
    assert_eq!(
        store
            .get_agent_run(queued.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        AgentRunStatus::Cancelled
    );

    let inbox = store
        .list_agent_run_inbox(AgentRunId::foreground_for_chat(chat.id))
        .await
        .unwrap();
    assert_eq!(
        inbox
            .iter()
            .find(|entry| entry.child_run_id == pending)
            .unwrap()
            .status,
        AgentRunInboxStatus::Cancelled
    );
    assert_eq!(
        inbox
            .iter()
            .find(|entry| entry.child_run_id == consumed)
            .unwrap()
            .status,
        AgentRunInboxStatus::Consumed
    );
    let queued_inbox = inbox
        .iter()
        .find(|entry| entry.child_run_id == queued.id)
        .unwrap();
    assert_eq!(queued_inbox.status, AgentRunInboxStatus::Cancelled);
    assert!(matches!(
        queued_inbox.result.payload,
        crate::AgentRunResultPayload::Cancelled {
            reason: AgentRunCancellationReason::ParentTurnFailed
        }
    ));

    store
        .finish_agent_run_cancellation(running.id, running_lease)
        .await
        .unwrap()
        .unwrap();
    let inbox = store
        .list_agent_run_inbox(AgentRunId::foreground_for_chat(chat.id))
        .await
        .unwrap();
    let running_inbox = inbox
        .iter()
        .find(|entry| entry.child_run_id == running.id)
        .unwrap();
    assert_eq!(running_inbox.status, AgentRunInboxStatus::Cancelled);
    assert!(matches!(
        running_inbox.result.payload,
        crate::AgentRunResultPayload::Cancelled {
            reason: AgentRunCancellationReason::ParentTurnFailed
        }
    ));
}

#[tokio::test]
async fn terminal_child_missing_its_inbox_is_corruption_not_a_silent_retirement() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (turn, lease) = live_turn_for_sandbox_test(&store, chat.id).await;
    let child = complete_child(&store, chat.id, "corrupt delivery").await;
    crate::db::entities::agent_run_inbox::Entity::delete_many()
        .filter(crate::db::entities::agent_run_inbox::Column::ChildRunId.eq(child.0))
        .exec(&store.conn)
        .await
        .unwrap();

    let output = output_for(&turn, chat.id);
    assert!(store
        .complete_turn(turn.id, lease, 0, Utc::now(), &output)
        .await
        .is_err());
    assert!(store
        .record_turn_failure(
            turn.id,
            lease,
            Utc::now(),
            TurnFailureRetry::Permanent,
            0,
            Usage::default(),
            "failed",
            None,
        )
        .await
        .is_err());
    assert_eq!(
        store.get_turn(turn.id).await.unwrap().unwrap().status,
        TurnRunStatus::Running
    );
}

#[tokio::test]
async fn terminal_child_corrupt_result_is_not_retired_by_parent_failure() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (turn, lease) = live_turn_for_sandbox_test(&store, chat.id).await;
    let child = complete_child(&store, chat.id, "corrupt result").await;
    let updated = crate::db::entities::agent_run_result::Entity::update_many()
        .col_expr(
            crate::db::entities::agent_run_result::Column::PayloadJson,
            sea_orm::sea_query::Expr::value("{not-json}"),
        )
        .filter(crate::db::entities::agent_run_result::Column::AgentRunId.eq(child.0))
        .exec(&store.conn)
        .await
        .unwrap();
    assert_eq!(updated.rows_affected, 1);

    assert!(store
        .record_turn_failure(
            turn.id,
            lease,
            Utc::now(),
            TurnFailureRetry::Permanent,
            0,
            Usage::default(),
            "failed",
            None,
        )
        .await
        .is_err());
    assert_eq!(
        store.get_turn(turn.id).await.unwrap().unwrap().status,
        TurnRunStatus::Running
    );
}
