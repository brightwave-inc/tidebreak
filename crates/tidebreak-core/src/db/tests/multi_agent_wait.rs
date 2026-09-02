use super::agent_run::{admit_sandbox_call_for_test, live_turn_for_sandbox_test};
use super::{sample_chat, temp_store, test_checkpoint_progress};
use crate::{
    AgentEvent, AgentRunId, AgentRunStatus, AgentRunWaitCondition, CallId, DeleteChatOutcome,
    ParkTurnForAgentRunWaitSetOutcome, RequestTurnCancellationOutcome,
    ResumeTurnForAgentRunWaitSetOutcome, Store, SubmitAgentRunResultOutcome, ToolCallExecution,
    ToolCallStatus, TurnAgentRunWaitStatus, TurnRunStatus, TurnSteerId,
};
use chrono::{Duration, Utc};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use sea_orm_migration::MigratorTrait;

#[allow(clippy::too_many_arguments)]
async fn park_wait_set_for_test(
    store: &crate::DbStore,
    wait_id: CallId,
    turn_id: crate::TurnId,
    child_run_ids: &[AgentRunId],
    condition: AgentRunWaitCondition,
    lease_token: uuid::Uuid,
    expected_steer_revision: i64,
    progress: crate::TurnCheckpointProgress,
    now: chrono::DateTime<Utc>,
) -> crate::Result<Option<ParkTurnForAgentRunWaitSetOutcome>> {
    let turn = store
        .get_turn(turn_id)
        .await?
        .ok_or_else(|| crate::AgentError::Store("test turn disappeared".into()))?;
    let existing = crate::db::entities::code_event::Entity::find()
        .filter(crate::db::entities::code_event::Column::LeaseToken.eq(lease_token))
        .filter(crate::db::entities::code_event::Column::AttemptEventOrdinal.eq(1))
        .one(&store.conn)
        .await
        .map_err(crate::db::store_err)?;
    if let Some(existing) = existing {
        let event = crate::chat_journal::decode_chat_event_required(existing.event)?;
        if existing.turn_id != Some(turn_id.0) || event != (AgentEvent::TurnStarted { turn_id }) {
            return Err(crate::AgentError::Store(
                "test lease has a different first event".into(),
            ));
        }
    } else {
        store
            .append_turn_event(
                turn.chat_id,
                turn_id,
                lease_token,
                1,
                now,
                &crate::AgentEvent::TurnStarted { turn_id },
            )
            .await?;
    }
    store
        .park_turn_for_agent_run_wait_set(
            &crate::AgentRunWaitSetCheckpointRequest {
                call_id: wait_id,
                origin_turn_id: turn_id,
                child_run_ids: child_run_ids.to_vec(),
                condition,
                lease_token,
                expected_steer_revision,
                provider_id: format!("provider-{wait_id}"),
                arguments: serde_json::json!({"agent_ids": child_run_ids}),
                event_ordinal: 2,
                progress,
            },
            now,
        )
        .await
}

async fn assert_cancelled_wait_shape(
    store: &crate::DbStore,
    wait_id: CallId,
    expected_result: &str,
) {
    let wait = crate::db::entities::turn_agent_run_wait_set::Entity::find_by_id(wait_id.0)
        .one(&store.conn)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(wait.status, TurnAgentRunWaitStatus::Cancelled.as_str());
    assert!(wait.closed_at.is_some());
    let event_seq = wait.event_seq.unwrap();
    let members = crate::db::entities::turn_agent_run_wait_member::Entity::find()
        .filter(crate::db::entities::turn_agent_run_wait_member::Column::WaitId.eq(wait_id.0))
        .all(&store.conn)
        .await
        .unwrap();
    assert!(!members.is_empty());
    assert!(members.iter().all(|member| !member.open));
    let call = store
        .list_tool_calls(crate::ChatId(wait.session_id))
        .await
        .unwrap()
        .into_iter()
        .find(|call| call.id == wait_id)
        .unwrap();
    assert_eq!(call.status, ToolCallStatus::Cancelled);
    assert_eq!(call.result.as_deref(), Some(expected_result));
    let event = crate::db::entities::code_event::Entity::find_by_id((wait.session_id, event_seq))
        .one(&store.conn)
        .await
        .unwrap()
        .unwrap();
    let event = crate::chat_journal::decode_chat_event_required(event.event).unwrap();
    assert_eq!(
        event,
        AgentEvent::ToolCallCompleted {
            call_id: wait_id,
            output: crate::ToolOutput::error(expected_result),
            action: None,
            result: None,
        }
    );
}

#[tokio::test]
async fn interrupt_steer_closes_wait_and_allows_the_same_pending_inbox_to_be_rewaited() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (running, lease) = live_turn_for_sandbox_test(&store, chat.id).await;
    let child = admit_sandbox_call_for_test(&store, chat.id, CallId::new(), "child").await;
    assert_eq!(complete_next_child(&store, "already ready").await, child.id);
    let first_wait_id = CallId::new();
    let first_request = crate::AgentRunWaitSetCheckpointRequest {
        call_id: first_wait_id,
        origin_turn_id: running.id,
        child_run_ids: vec![child.id],
        condition: AgentRunWaitCondition::All,
        lease_token: lease,
        expected_steer_revision: running.steer_revision,
        provider_id: "provider-first-wait".into(),
        arguments: serde_json::json!({"agent_ids": [child.id]}),
        event_ordinal: 2,
        progress: test_checkpoint_progress(),
    };
    store
        .append_turn_event(
            chat.id,
            running.id,
            lease,
            1,
            Utc::now(),
            &AgentEvent::TurnStarted {
                turn_id: running.id,
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        store
            .park_turn_for_agent_run_wait_set(&first_request, Utc::now())
            .await
            .unwrap(),
        Some(ParkTurnForAgentRunWaitSetOutcome::Parked { .. })
    ));

    let steer_id = TurnSteerId::new();
    store
        .accept_turn_steer(
            steer_id,
            running.id,
            chat.id,
            "use the result differently",
            true,
        )
        .await
        .unwrap();
    assert_eq!(
        store.get_turn(running.id).await.unwrap().unwrap().status,
        TurnRunStatus::Resuming
    );
    let calls = store.list_tool_calls(chat.id).await.unwrap();
    let interrupted = calls.iter().find(|call| call.id == first_wait_id).unwrap();
    assert_eq!(interrupted.status, ToolCallStatus::Cancelled);
    assert_eq!(interrupted.execution, ToolCallExecution::Orchestration);
    assert_cancelled_wait_shape(
        &store,
        first_wait_id,
        crate::agent_tools::WAIT_INTERRUPTED_BY_STEER_RESULT,
    )
    .await;
    assert!(matches!(
        store
            .park_turn_for_agent_run_wait_set(&first_request, Utc::now())
            .await
            .unwrap(),
        Some(ParkTurnForAgentRunWaitSetOutcome::Existing { .. })
    ));
    let inbox = store
        .list_agent_run_inbox(running.agent_run_id)
        .await
        .unwrap();
    assert_eq!(inbox[0].status, crate::AgentRunInboxStatus::Pending);

    let resumed_lease = uuid::Uuid::new_v4();
    let resumed = store
        .claim_turn(resumed_lease, Utc::now(), Utc::now() + Duration::minutes(5))
        .await
        .unwrap()
        .turn
        .unwrap();
    store
        .append_turn_event(
            chat.id,
            resumed.id,
            resumed_lease,
            1,
            Utc::now(),
            &AgentEvent::TurnStarted {
                turn_id: resumed.id,
            },
        )
        .await
        .unwrap();
    store
        .apply_turn_steer(
            resumed.id,
            resumed_lease,
            steer_id,
            2,
            None,
            &[],
            Utc::now(),
        )
        .await
        .unwrap()
        .unwrap();
    let applied = store.get_turn(running.id).await.unwrap().unwrap();
    let second_wait_id = CallId::new();
    let second_request = crate::AgentRunWaitSetCheckpointRequest {
        call_id: second_wait_id,
        origin_turn_id: applied.id,
        child_run_ids: vec![child.id],
        condition: AgentRunWaitCondition::All,
        lease_token: resumed_lease,
        expected_steer_revision: applied.steer_revision,
        provider_id: "provider-second-wait".into(),
        arguments: serde_json::json!({"agent_ids": [child.id]}),
        event_ordinal: 3,
        progress: test_checkpoint_progress(),
    };
    assert!(matches!(
        store
            .park_turn_for_agent_run_wait_set(&second_request, Utc::now())
            .await
            .unwrap(),
        Some(ParkTurnForAgentRunWaitSetOutcome::Parked { .. })
    ));
    assert!(matches!(
        store
            .resume_turn_for_agent_run_wait_set(second_wait_id, uuid::Uuid::new_v4())
            .await
            .unwrap(),
        Some(ResumeTurnForAgentRunWaitSetOutcome::Resumed { .. })
    ));
}

#[tokio::test]
async fn interrupt_steer_rolls_back_every_receipt_when_wait_close_loses_a_member() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (running, lease) = live_turn_for_sandbox_test(&store, chat.id).await;
    let child = admit_sandbox_call_for_test(&store, chat.id, CallId::new(), "child").await;
    let wait_id = CallId::new();
    park_wait_set_for_test(
        &store,
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

    // Simulate a storage-side race after the pending tool call has been
    // terminalized but before the member close count is checked. The
    // transaction must not commit a cancelled call/event around an open wait.
    sea_orm::ConnectionTrait::execute_unprepared(
        &store.conn,
        r#"CREATE TRIGGER lose_wait_member_after_tool_close
AFTER UPDATE OF status ON tool_call
BEGIN
  UPDATE turn_agent_run_wait_member SET open = FALSE WHERE wait_id = NEW.id;
END"#,
    )
    .await
    .unwrap();

    let steer_id = TurnSteerId::new();
    let error = store
        .accept_turn_steer(steer_id, running.id, chat.id, "interrupt", true)
        .await
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("changed while closing its ordered receipt"));

    let wait = crate::db::entities::turn_agent_run_wait_set::Entity::find_by_id(wait_id.0)
        .one(&store.conn)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(wait.status, TurnAgentRunWaitStatus::Waiting.as_str());
    assert!(wait.closed_at.is_none());
    assert!(wait.event_seq.is_none());
    let member = crate::db::entities::turn_agent_run_wait_member::Entity::find()
        .filter(crate::db::entities::turn_agent_run_wait_member::Column::WaitId.eq(wait_id.0))
        .one(&store.conn)
        .await
        .unwrap()
        .unwrap();
    assert!(member.open);
    let call = store
        .list_tool_calls(chat.id)
        .await
        .unwrap()
        .into_iter()
        .find(|call| call.id == wait_id)
        .unwrap();
    assert_eq!(call.status, ToolCallStatus::Pending);
    assert!(call.result.is_none());
    assert!(
        crate::db::entities::code_turn_steer::Entity::find_by_id(steer_id.0)
            .one(&store.conn)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store.get_turn(running.id).await.unwrap().unwrap().status,
        TurnRunStatus::WaitingForAgentRun
    );
}

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
async fn recovery_scan_becomes_ready_only_after_the_last_ordered_child() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (running, lease) = live_turn_for_sandbox_test(&store, chat.id).await;
    let child_a = admit_sandbox_call_for_test(&store, chat.id, CallId::new(), "a").await;
    let child_b = admit_sandbox_call_for_test(&store, chat.id, CallId::new(), "b").await;
    let wait_id = CallId::new();
    park_wait_set_for_test(
        &store,
        wait_id,
        running.id,
        &[child_b.id, child_a.id],
        AgentRunWaitCondition::All,
        lease,
        running.steer_revision,
        test_checkpoint_progress(),
        Utc::now(),
    )
    .await
    .unwrap();

    complete_next_child(&store, "first").await;
    assert!(store
        .list_ready_agent_run_wait_set_candidates(8)
        .await
        .unwrap()
        .is_empty());
    complete_next_child(&store, "second").await;
    let candidates = store
        .list_ready_agent_run_wait_set_candidates(8)
        .await
        .unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].wait_id, wait_id);
    let delivered_at = store
        .list_agent_run_inbox(running.agent_run_id)
        .await
        .unwrap()
        .into_iter()
        .map(|entry| entry.delivered_at)
        .max()
        .unwrap();
    assert_eq!(candidates[0].ready_at, delivered_at);
    assert!(store
        .list_ready_agent_run_wait_set_candidates(0)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn recovery_scan_includes_children_delivered_before_the_wait_was_parked() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (running, lease) = live_turn_for_sandbox_test(&store, chat.id).await;
    let child_a = admit_sandbox_call_for_test(&store, chat.id, CallId::new(), "a").await;
    let child_b = admit_sandbox_call_for_test(&store, chat.id, CallId::new(), "b").await;
    assert_eq!(complete_next_child(&store, "first").await, child_a.id);
    assert_eq!(complete_next_child(&store, "second").await, child_b.id);

    let wait_id = CallId::new();
    park_wait_set_for_test(
        &store,
        wait_id,
        running.id,
        &[child_b.id, child_a.id],
        AgentRunWaitCondition::All,
        lease,
        running.steer_revision,
        test_checkpoint_progress(),
        Utc::now(),
    )
    .await
    .unwrap();

    assert_eq!(
        store
            .list_ready_agent_run_wait_set_candidates(1)
            .await
            .unwrap()
            .into_iter()
            .map(|candidate| candidate.wait_id)
            .collect::<Vec<_>>(),
        vec![wait_id]
    );
}

#[tokio::test]
async fn recovery_scan_excludes_malformed_member_ownership() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (running, lease) = live_turn_for_sandbox_test(&store, chat.id).await;
    let child = admit_sandbox_call_for_test(&store, chat.id, CallId::new(), "child").await;
    let wait_id = CallId::new();
    park_wait_set_for_test(
        &store,
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
    complete_next_child(&store, "done").await;
    crate::db::entities::turn_agent_run_wait_member::Entity::update_many()
        .col_expr(
            crate::db::entities::turn_agent_run_wait_member::Column::Position,
            sea_orm::sea_query::Expr::value(1_i16),
        )
        .filter(crate::db::entities::turn_agent_run_wait_member::Column::WaitId.eq(wait_id.0))
        .exec(&store.conn)
        .await
        .unwrap();

    // A corrupt older set must be removed before LIMIT, not consume the only
    // recovery slot and starve a later coherent set.
    let later_chat = sample_chat();
    store.create_chat(&later_chat).await.unwrap();
    let (later_turn, later_lease) = live_turn_for_sandbox_test(&store, later_chat.id).await;
    let later_child =
        admit_sandbox_call_for_test(&store, later_chat.id, CallId::new(), "later").await;
    let later_wait_id = CallId::new();
    park_wait_set_for_test(
        &store,
        later_wait_id,
        later_turn.id,
        &[later_child.id],
        AgentRunWaitCondition::All,
        later_lease,
        later_turn.steer_revision,
        test_checkpoint_progress(),
        Utc::now(),
    )
    .await
    .unwrap();
    assert_eq!(
        complete_next_child(&store, "later done").await,
        later_child.id
    );

    assert_eq!(
        store
            .list_ready_agent_run_wait_set_candidates(1)
            .await
            .unwrap()
            .into_iter()
            .map(|candidate| candidate.wait_id)
            .collect::<Vec<_>>(),
        vec![later_wait_id]
    );
}

#[tokio::test]
async fn concurrent_recovery_tokens_have_one_wait_set_winner() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (running, lease) = live_turn_for_sandbox_test(&store, chat.id).await;
    let child = admit_sandbox_call_for_test(&store, chat.id, CallId::new(), "child").await;
    let wait_id = CallId::new();
    park_wait_set_for_test(
        &store,
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
    complete_next_child(&store, "done").await;

    let first = store.clone();
    let second = store.clone();
    let (a, b) = tokio::join!(
        first.resume_turn_for_agent_run_wait_set(wait_id, uuid::Uuid::new_v4()),
        second.resume_turn_for_agent_run_wait_set(wait_id, uuid::Uuid::new_v4())
    );
    assert_eq!(
        [a.unwrap(), b.unwrap()]
            .into_iter()
            .filter(|outcome| matches!(
                outcome,
                Some(ResumeTurnForAgentRunWaitSetOutcome::Resumed { .. })
            ))
            .count(),
        1
    );
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

    let parked = park_wait_set_for_test(
        &store,
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
    let ParkTurnForAgentRunWaitSetOutcome::Parked {
        wait,
        call: pending_call,
        ..
    } = parked
    else {
        panic!("unexpected park outcome: {parked:?}")
    };
    assert_eq!(wait.child_run_ids, requested);
    assert_eq!(wait.status, TurnAgentRunWaitStatus::Waiting);
    assert_eq!(pending_call.name, crate::agent_tools::WAIT_FOR_AGENTS_TOOL);
    assert_eq!(pending_call.execution, ToolCallExecution::Orchestration);
    assert_eq!(pending_call.status, ToolCallStatus::Pending);
    assert_eq!(pending_call.provider_id, format!("provider-{wait_id}"));
    assert_eq!(
        pending_call.arguments,
        serde_json::json!({"agent_ids": requested})
    );
    assert_eq!(pending_call.result, None);
    assert_eq!(pending_call.resolved_at, None);
    assert!(matches!(
        park_wait_set_for_test(
            &store,
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
    let (results, completed_call, completed_event) = match resumed {
        ResumeTurnForAgentRunWaitSetOutcome::Resumed {
            turn,
            wait,
            results,
            call,
            event,
        } => {
            assert_eq!(turn.status, TurnRunStatus::Resuming);
            assert_eq!(turn.model_steps, progress.model_steps);
            assert_eq!(wait.child_run_ids, requested);
            (results, call, event)
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
    assert_eq!(completed_call.status, ToolCallStatus::Completed);
    assert_eq!(completed_call.execution, ToolCallExecution::Orchestration);
    let AgentEvent::ToolCallCompleted {
        call_id, output, ..
    } = completed_event.event
    else {
        panic!("wait completion emitted the wrong event")
    };
    assert_eq!(call_id, wait_id);
    assert!(!output.is_error);
    assert_eq!(output.content, completed_call.result.clone().unwrap());
    let decoded: serde_json::Value = serde_json::from_str(&output.content).unwrap();
    let decoded_results = decoded["results"].as_array().unwrap();
    assert_eq!(decoded_results.len(), requested.len());
    for (decoded, entry) in decoded_results.iter().zip(&results) {
        assert_eq!(decoded["agent_id"], entry.child_run_id.to_string());
        assert_eq!(decoded["result"]["text"], entry.result.text);
        assert_eq!(decoded["truncated"], false);
    }
    assert!(matches!(
        park_wait_set_for_test(
            &store,
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

    let continuation_lease = uuid::Uuid::new_v4();
    let reclaimed = store
        .claim_turn(
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
    assert!(system_messages.is_empty());
}

#[tokio::test]
async fn cancelled_child_is_a_terminal_multi_wait_delivery() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (running, lease) = live_turn_for_sandbox_test(&store, chat.id).await;
    let child = admit_sandbox_call_for_test(&store, chat.id, CallId::new(), "cancel me").await;
    let wait_id = CallId::new();
    park_wait_set_for_test(
        &store,
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
    park_wait_set_for_test(
        &store,
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
    assert!(wait.event_seq.is_some());
    let call = store
        .list_tool_calls(chat.id)
        .await
        .unwrap()
        .into_iter()
        .find(|call| call.id == wait_id)
        .unwrap();
    assert_eq!(call.status, ToolCallStatus::Cancelled);
    assert_cancelled_wait_shape(
        &store,
        wait_id,
        crate::agent_tools::WAIT_CANCELLED_WITH_TURN_RESULT,
    )
    .await;
    assert!(matches!(
        park_wait_set_for_test(
            &store,
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
        .unwrap(),
        Some(ParkTurnForAgentRunWaitSetOutcome::Existing { .. })
    ));
    for child in [child_a.id, child_b.id] {
        assert_eq!(
            store.get_agent_run(child).await.unwrap().unwrap().status,
            AgentRunStatus::Cancelled
        );
    }
    assert!(matches!(
        store.delete_chat(chat.id).await.unwrap(),
        DeleteChatOutcome::Deleted { .. }
    ));
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

    assert!(park_wait_set_for_test(
        &store,
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
        park_wait_set_for_test(
            &first_store,
            shared_wait_id,
            first_turn.id,
            &first_members,
            AgentRunWaitCondition::All,
            first_lease,
            first_turn.steer_revision,
            test_checkpoint_progress(),
            Utc::now(),
        ),
        park_wait_set_for_test(
            &second_store,
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
        let current = store.get_turn(turn.id).await.unwrap().unwrap();
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
    park_wait_set_for_test(
        &store,
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
        parent_run_id: sea_orm::Set(crate::id::AgentRunId::foreground_for_chat(other_chat.id).0),
        origin_turn_id: sea_orm::Set(other_turn.id.0),
        chat_id: sea_orm::Set(other_chat.id.0),
        open: sea_orm::Set(true),
    };
    assert!(sea_orm::ActiveModelTrait::insert(corrupt, &store.conn)
        .await
        .is_err());
}

#[tokio::test]
async fn a_consumed_wait_set_child_cannot_be_reparked_on_a_new_wait() {
    let (_dir, store) = temp_store().await;

    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (turn, lease) = live_turn_for_sandbox_test(&store, chat.id).await;
    let child = admit_sandbox_call_for_test(&store, chat.id, CallId::new(), "child").await;
    let wait_id = CallId::new();
    park_wait_set_for_test(
        &store,
        wait_id,
        turn.id,
        &[child.id],
        AgentRunWaitCondition::All,
        lease,
        turn.steer_revision,
        test_checkpoint_progress(),
        Utc::now(),
    )
    .await
    .unwrap();
    assert_eq!(complete_next_child(&store, "result").await, child.id);
    store
        .resume_turn_for_agent_run_wait_set(wait_id, uuid::Uuid::new_v4())
        .await
        .unwrap()
        .expect("ready wait set should resume");

    let reclaimed_lease = uuid::Uuid::new_v4();
    let reclaimed = store
        .claim_turn(
            reclaimed_lease,
            Utc::now(),
            Utc::now() + Duration::minutes(5),
        )
        .await
        .unwrap()
        .turn
        .unwrap();
    // The child's delivery is consumed, so it can never back a second wait.
    assert!(matches!(
        park_wait_set_for_test(
            &store,
            CallId::new(),
            reclaimed.id,
            &[child.id],
            AgentRunWaitCondition::All,
            reclaimed_lease,
            reclaimed.steer_revision,
            test_checkpoint_progress(),
            Utc::now(),
        )
        .await
        .unwrap(),
        Some(ParkTurnForAgentRunWaitSetOutcome::IdentityConflict)
    ));
}

#[tokio::test]
#[ignore = "one-turn-lane down is a no-op"]
async fn baseline_migration_rolls_back_multi_wait_dependencies_in_fk_order() {
    let (_dir, store) = temp_store().await;
    crate::db::migration::Migrator::down(&store.conn, None)
        .await
        .unwrap();
}
