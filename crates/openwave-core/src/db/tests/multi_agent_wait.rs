use super::agent_run::{admit_sandbox_call_for_test, live_turn_for_sandbox_test};
use super::{sample_chat, temp_store, test_checkpoint_progress};
use crate::{
    AgentRunId, AgentRunStatus, AgentRunWaitCondition, CallId, DeleteChatOutcome,
    ParkTurnForAgentRunWaitSetOutcome, RequestTurnCancellationOutcome,
    ResumeTurnForAgentRunWaitSetOutcome, Store, SubmitAgentRunResultOutcome,
    TurnAgentRunWaitStatus, TurnRunStatus,
};
use chrono::{Duration, Utc};
use sea_orm::EntityTrait;
use sea_orm_migration::MigratorTrait;

async fn complete_next_child(store: &crate::DbStore, text: &str) -> AgentRunId {
    let lease = uuid::Uuid::new_v4();
    let child = store
        .claim_agent_run(lease, Duration::minutes(5), 4, 4)
        .await
        .unwrap()
        .expect("one sandbox child should claim");
    match store
        .submit_agent_run_result(child.id, lease, text)
        .await
        .unwrap()
        .expect("sandbox result should commit")
    {
        SubmitAgentRunResultOutcome::Completed(result) => assert_eq!(result.text, text),
        outcome => panic!("unexpected submission outcome: {outcome:?}"),
    }
    child.id
}

#[tokio::test]
async fn ordered_all_wait_consumes_once_and_exactly_recovers_after_reclaim() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (running, lease) = live_turn_for_sandbox_test(&store, chat.id).await;
    let child_a = admit_sandbox_call_for_test(&store, chat.id, CallId::new(), "child a").await;
    let child_b = admit_sandbox_call_for_test(&store, chat.id, CallId::new(), "child b").await;
    let requested = [child_b.id, child_a.id];
    let wait_id = CallId::new();
    let progress = test_checkpoint_progress();

    let parked = store
        .park_turn_for_agent_run_wait_set(
            wait_id,
            running.id,
            &requested,
            AgentRunWaitCondition::All,
            lease,
            running.steer_revision,
            progress,
            Utc::now(),
        )
        .await
        .unwrap()
        .expect("wait should resolve");
    assert!(matches!(
        parked,
        ParkTurnForAgentRunWaitSetOutcome::Parked { ref wait, .. }
            if wait.child_run_ids == requested
                && wait.status == TurnAgentRunWaitStatus::Waiting
    ));
    assert!(matches!(
        store
            .park_turn_for_agent_run_wait_set(
                wait_id,
                running.id,
                &requested,
                AgentRunWaitCondition::All,
                lease,
                running.steer_revision,
                progress,
                Utc::now(),
            )
            .await
            .unwrap(),
        Some(ParkTurnForAgentRunWaitSetOutcome::Existing { .. })
    ));

    let first_finished = complete_next_child(&store, "first completion").await;
    let resume_token = uuid::Uuid::new_v4();
    assert!(matches!(
        store
            .resume_turn_for_agent_run_wait_set(wait_id, resume_token)
            .await
            .unwrap(),
        Some(ResumeTurnForAgentRunWaitSetOutcome::NotReady(_))
    ));
    let second_finished = complete_next_child(&store, "second completion").await;
    assert_ne!(first_finished, second_finished);

    let resumed = store
        .resume_turn_for_agent_run_wait_set(wait_id, resume_token)
        .await
        .unwrap()
        .expect("satisfied wait should resume");
    let results = match resumed {
        ResumeTurnForAgentRunWaitSetOutcome::Resumed {
            turn,
            wait,
            results,
        } => {
            assert_eq!(turn.status, TurnRunStatus::Resuming);
            assert_eq!(turn.model_steps, progress.model_steps);
            assert_eq!(wait.child_run_ids, requested);
            results
        }
        outcome => panic!("unexpected resume outcome: {outcome:?}"),
    };
    assert_eq!(
        results
            .iter()
            .map(|entry| entry.child_run_id)
            .collect::<Vec<_>>(),
        requested
    );
    assert!(results.iter().all(|entry| {
        entry.consumed_lease_token == Some(resume_token)
            && entry.status == crate::AgentRunInboxStatus::Consumed
    }));

    let continuation_lease = uuid::Uuid::new_v4();
    let reclaimed = store
        .claim_turn_run(
            continuation_lease,
            Utc::now(),
            Utc::now() + Duration::minutes(5),
        )
        .await
        .unwrap()
        .turn
        .expect("resumed turn should reclaim");
    assert_eq!(reclaimed.id, running.id);
    assert_eq!(reclaimed.status, TurnRunStatus::Running);
    assert_eq!(reclaimed.model_steps, progress.model_steps);

    match store
        .resume_turn_for_agent_run_wait_set(wait_id, resume_token)
        .await
        .unwrap()
        .expect("exact retry should recover after reclaim")
    {
        ResumeTurnForAgentRunWaitSetOutcome::Existing { turn, results, .. } => {
            assert_eq!(turn.status, TurnRunStatus::Running);
            assert_eq!(turn.lease_token, Some(continuation_lease));
            assert_eq!(
                results
                    .iter()
                    .map(|entry| entry.child_run_id)
                    .collect::<Vec<_>>(),
                requested
            );
        }
        outcome => panic!("unexpected exact recovery: {outcome:?}"),
    }
    assert!(store
        .resume_turn_for_agent_run_wait_set(wait_id, uuid::Uuid::new_v4())
        .await
        .unwrap()
        .is_none());

    let system_messages = store
        .list_messages(chat.id)
        .await
        .unwrap()
        .into_iter()
        .filter(|message| message.role == crate::Role::System)
        .collect::<Vec<_>>();
    assert_eq!(system_messages.len(), 2);
    let requested_text = requested
        .iter()
        .map(|child_id| {
            results
                .iter()
                .find(|entry| entry.child_run_id == *child_id)
                .unwrap()
                .result
                .text
                .clone()
        })
        .collect::<Vec<_>>();
    assert!(system_messages[0].content.ends_with(&requested_text[0]));
    assert!(system_messages[1].content.ends_with(&requested_text[1]));
}

#[tokio::test]
async fn cancelled_child_is_a_terminal_multi_wait_delivery() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (running, lease) = live_turn_for_sandbox_test(&store, chat.id).await;
    let child = admit_sandbox_call_for_test(&store, chat.id, CallId::new(), "cancel me").await;
    let wait_id = CallId::new();
    store
        .park_turn_for_agent_run_wait_set(
            wait_id,
            running.id,
            &[child.id],
            AgentRunWaitCondition::All,
            lease,
            running.steer_revision,
            test_checkpoint_progress(),
            Utc::now(),
        )
        .await
        .unwrap();
    store
        .request_agent_run_cancellation(child.id)
        .await
        .unwrap()
        .expect("queued child should cancel");

    let outcome = store
        .resume_turn_for_agent_run_wait_set(wait_id, uuid::Uuid::new_v4())
        .await
        .unwrap()
        .expect("cancelled child delivery completes the All wait");
    let ResumeTurnForAgentRunWaitSetOutcome::Resumed { results, .. } = outcome else {
        panic!("unexpected cancellation resume outcome: {outcome:?}")
    };
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].child_run_id, child.id);
    assert!(matches!(
        results[0].result.payload,
        crate::AgentRunResultPayload::Cancelled {
            reason: crate::AgentRunCancellationReason::Requested
        }
    ));
}

#[tokio::test]
async fn cancelling_a_multi_wait_fences_children_and_terminal_chat_deletes_cleanly() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (running, lease) = live_turn_for_sandbox_test(&store, chat.id).await;
    let child_a = admit_sandbox_call_for_test(&store, chat.id, CallId::new(), "child a").await;
    let child_b = admit_sandbox_call_for_test(&store, chat.id, CallId::new(), "child b").await;
    let wait_id = CallId::new();
    store
        .park_turn_for_agent_run_wait_set(
            wait_id,
            running.id,
            &[child_a.id, child_b.id],
            AgentRunWaitCondition::All,
            lease,
            running.steer_revision,
            test_checkpoint_progress(),
            Utc::now(),
        )
        .await
        .unwrap();

    assert!(matches!(
        store
            .request_turn_cancellation(running.id, Utc::now())
            .await
            .unwrap(),
        Some(RequestTurnCancellationOutcome::Cancelled(_))
    ));
    let wait = crate::db::entities::turn_agent_run_wait_set::Entity::find_by_id(wait_id.0)
        .one(&store.conn)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(wait.status, TurnAgentRunWaitStatus::Cancelled.as_str());
    assert!(wait.closed_at.is_some());
    for child in [child_a.id, child_b.id] {
        assert_eq!(
            store.get_agent_run(child).await.unwrap().unwrap().status,
            AgentRunStatus::Cancelled
        );
    }
    assert_eq!(
        store.delete_chat(chat.id).await.unwrap(),
        DeleteChatOutcome::Deleted
    );
}

#[tokio::test]
async fn wait_set_rejects_duplicate_members_and_concurrent_cross_chat_identity_collision() {
    let (_dir, store) = temp_store().await;
    let first_chat = sample_chat();
    let second_chat = sample_chat();
    store.create_chat(&first_chat).await.unwrap();
    store.create_chat(&second_chat).await.unwrap();
    let (first_turn, first_lease) = live_turn_for_sandbox_test(&store, first_chat.id).await;
    let (second_turn, second_lease) = live_turn_for_sandbox_test(&store, second_chat.id).await;
    let first_child =
        admit_sandbox_call_for_test(&store, first_chat.id, CallId::new(), "first").await;
    let second_child =
        admit_sandbox_call_for_test(&store, second_chat.id, CallId::new(), "second").await;

    assert!(store
        .park_turn_for_agent_run_wait_set(
            CallId::new(),
            first_turn.id,
            &[first_child.id, first_child.id],
            AgentRunWaitCondition::All,
            first_lease,
            first_turn.steer_revision,
            test_checkpoint_progress(),
            Utc::now(),
        )
        .await
        .is_err());

    let shared_wait_id = CallId::new();
    let first_store = store.clone();
    let second_store = store.clone();
    let first_members = [first_child.id];
    let second_members = [second_child.id];
    let (first, second) = tokio::join!(
        first_store.park_turn_for_agent_run_wait_set(
            shared_wait_id,
            first_turn.id,
            &first_members,
            AgentRunWaitCondition::All,
            first_lease,
            first_turn.steer_revision,
            test_checkpoint_progress(),
            Utc::now(),
        ),
        second_store.park_turn_for_agent_run_wait_set(
            shared_wait_id,
            second_turn.id,
            &second_members,
            AgentRunWaitCondition::All,
            second_lease,
            second_turn.steer_revision,
            test_checkpoint_progress(),
            Utc::now(),
        )
    );
    let outcomes = [first.unwrap().unwrap(), second.unwrap().unwrap()];
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, ParkTurnForAgentRunWaitSetOutcome::Parked { .. }))
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(
                outcome,
                ParkTurnForAgentRunWaitSetOutcome::IdentityConflict
            ))
            .count(),
        1
    );

    // The loser remains a valid running turn; cancellation cleans both sides.
    for turn in [first_turn, second_turn] {
        let current = store.get_turn_run(turn.id).await.unwrap().unwrap();
        store
            .request_turn_cancellation(turn.id, Utc::now().max(current.updated_at))
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn wait_member_composite_foreign_key_rejects_cross_turn_ownership() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (running, lease) = live_turn_for_sandbox_test(&store, chat.id).await;
    let child = admit_sandbox_call_for_test(&store, chat.id, CallId::new(), "owned").await;
    let other_chat = sample_chat();
    store.create_chat(&other_chat).await.unwrap();
    let (other_turn, _other_lease) = live_turn_for_sandbox_test(&store, other_chat.id).await;
    let other_child =
        admit_sandbox_call_for_test(&store, other_chat.id, CallId::new(), "other owner").await;
    let wait_id = CallId::new();
    store
        .park_turn_for_agent_run_wait_set(
            wait_id,
            running.id,
            &[child.id],
            AgentRunWaitCondition::All,
            lease,
            running.steer_revision,
            test_checkpoint_progress(),
            Utc::now(),
        )
        .await
        .unwrap();
    let corrupt = crate::db::entities::turn_agent_run_wait_member::ActiveModel {
        wait_id: sea_orm::Set(wait_id.0),
        position: sea_orm::Set(1),
        child_run_id: sea_orm::Set(other_child.id.0),
        parent_run_id: sea_orm::Set(other_turn.agent_run_id.0),
        origin_turn_id: sea_orm::Set(other_turn.id.0),
        chat_id: sea_orm::Set(other_chat.id.0),
    };
    assert!(sea_orm::ActiveModelTrait::insert(corrupt, &store.conn)
        .await
        .is_err());
}

#[tokio::test]
async fn legacy_and_multi_waits_cannot_reconsume_the_same_child_delivery() {
    let (_dir, store) = temp_store().await;

    let legacy_chat = sample_chat();
    store.create_chat(&legacy_chat).await.unwrap();
    let (legacy_turn, legacy_lease) = live_turn_for_sandbox_test(&store, legacy_chat.id).await;
    let legacy_child =
        admit_sandbox_call_for_test(&store, legacy_chat.id, CallId::new(), "legacy child").await;
    store
        .park_turn_for_agent_run_inbox(
            legacy_turn.id,
            legacy_child.id,
            legacy_lease,
            legacy_turn.steer_revision,
            test_checkpoint_progress(),
            Utc::now(),
        )
        .await
        .unwrap();
    assert_eq!(
        complete_next_child(&store, "legacy result").await,
        legacy_child.id
    );
    let inbox_lease = uuid::Uuid::new_v4();
    store
        .claim_agent_run_inbox_entry(
            legacy_turn.agent_run_id,
            legacy_child.id,
            inbox_lease,
            Duration::minutes(5),
        )
        .await
        .unwrap();
    store
        .consume_agent_run_inbox_entry_and_resume_turn(
            legacy_turn.agent_run_id,
            legacy_child.id,
            inbox_lease,
        )
        .await
        .unwrap();
    let next_lease = uuid::Uuid::new_v4();
    let next = store
        .claim_turn_run(next_lease, Utc::now(), Utc::now() + Duration::minutes(5))
        .await
        .unwrap()
        .turn
        .unwrap();
    assert!(matches!(
        store
            .park_turn_for_agent_run_wait_set(
                CallId::new(),
                next.id,
                &[legacy_child.id],
                AgentRunWaitCondition::All,
                next_lease,
                next.steer_revision,
                test_checkpoint_progress(),
                Utc::now(),
            )
            .await
            .unwrap(),
        Some(ParkTurnForAgentRunWaitSetOutcome::IdentityConflict)
    ));

    let multi_chat = sample_chat();
    store.create_chat(&multi_chat).await.unwrap();
    let (multi_turn, multi_lease) = live_turn_for_sandbox_test(&store, multi_chat.id).await;
    let multi_child =
        admit_sandbox_call_for_test(&store, multi_chat.id, CallId::new(), "multi child").await;
    let wait_id = CallId::new();
    store
        .park_turn_for_agent_run_wait_set(
            wait_id,
            multi_turn.id,
            &[multi_child.id],
            AgentRunWaitCondition::All,
            multi_lease,
            multi_turn.steer_revision,
            test_checkpoint_progress(),
            Utc::now(),
        )
        .await
        .unwrap();
    assert_eq!(
        complete_next_child(&store, "multi result").await,
        multi_child.id
    );
    store
        .resume_turn_for_agent_run_wait_set(wait_id, uuid::Uuid::new_v4())
        .await
        .unwrap();
    let reclaimed_lease = uuid::Uuid::new_v4();
    let reclaimed = store
        .claim_turn_run(
            reclaimed_lease,
            Utc::now(),
            Utc::now() + Duration::minutes(5),
        )
        .await
        .unwrap()
        .turn
        .unwrap();
    assert!(matches!(
        store
            .park_turn_for_agent_run_inbox(
                reclaimed.id,
                multi_child.id,
                reclaimed_lease,
                reclaimed.steer_revision,
                test_checkpoint_progress(),
                Utc::now(),
            )
            .await
            .unwrap(),
        Some(crate::ParkTurnForAgentRunInboxOutcome::IdentityConflict)
    ));
}

#[tokio::test]
async fn baseline_migration_rolls_back_multi_wait_dependencies_in_fk_order() {
    let (_dir, store) = temp_store().await;
    crate::db::migration::Migrator::down(&store.conn, None)
        .await
        .unwrap();
}
