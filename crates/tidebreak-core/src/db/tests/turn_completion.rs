use super::*;

#[tokio::test]
async fn turn_completion_atomically_persists_exact_output_and_recovers_retries() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    let accepted = match store
        .accept_turn(turn_id, chat.id, "gpt-5", "hello")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    let claimed_at = accepted.available_at + chrono::Duration::seconds(1);
    let lease_expires_at = claimed_at + chrono::Duration::minutes(1);
    let lease_token = uuid::Uuid::new_v4();
    store
        .claim_turn(lease_token, claimed_at, lease_expires_at)
        .await
        .unwrap()
        .turn
        .unwrap();
    let output = Message {
        id: MessageId::new(),
        chat_id: chat.id,
        turn_id,
        role: Role::Assistant,
        reasoning: Default::default(),
        content: "final answer".into(),
        llm_content: None,
        created_at: claimed_at + chrono::Duration::nanoseconds(1_234_567),
    };

    assert_eq!(
        store
            .complete_turn(turn_id, uuid::Uuid::new_v4(), 0, output.created_at, &output)
            .await
            .unwrap(),
        None
    );
    assert_eq!(store.list_messages(chat.id).await.unwrap().len(), 1);

    let mut invalid = output.clone();
    invalid.role = Role::User;
    assert!(store
        .complete_turn(turn_id, lease_token, 0, output.created_at, &invalid)
        .await
        .is_err());
    invalid = output.clone();
    invalid.turn_id = TurnId::new();
    assert!(store
        .complete_turn(turn_id, lease_token, 0, output.created_at, &invalid)
        .await
        .is_err());
    invalid = output.clone();
    invalid.chat_id = SessionId::new();
    assert!(store
        .complete_turn(turn_id, lease_token, 0, output.created_at, &invalid)
        .await
        .is_err());
    assert_eq!(store.list_messages(chat.id).await.unwrap().len(), 1);

    let CompleteTurnRunOutcome::Completed(completed) = store
        .complete_turn(turn_id, lease_token, 0, output.created_at, &output)
        .await
        .unwrap()
        .unwrap()
    else {
        panic!("first exact completion must commit")
    };
    let canonical_completed_at =
        DateTime::<Utc>::from_timestamp_micros(output.created_at.timestamp_micros()).unwrap();
    assert_eq!(completed.status, TurnRunStatus::Completed);
    assert_eq!(completed.output_message_id, Some(output.id));
    assert_eq!(completed.lease_token, None);
    assert_eq!(completed.lease_expires_at, None);
    assert_eq!(completed.finished_at, Some(canonical_completed_at));
    assert_eq!(completed.updated_at, canonical_completed_at);
    let messages = store.list_messages(chat.id).await.unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[1].id, output.id);
    assert_eq!(messages[1].content, output.content);
    assert_eq!(messages[1].created_at, canonical_completed_at);

    assert_eq!(
        store
            .complete_turn(
                turn_id,
                lease_token,
                0,
                lease_expires_at + chrono::Duration::seconds(1),
                &output,
            )
            .await
            .unwrap(),
        Some(CompleteTurnRunOutcome::Existing(completed.clone()))
    );
    assert_eq!(store.list_messages(chat.id).await.unwrap().len(), 2);

    let mut mismatched = output.clone();
    mismatched.content = "different answer".into();
    assert!(store
        .complete_turn(turn_id, lease_token, 0, lease_expires_at, &mismatched)
        .await
        .is_err());
    mismatched = output.clone();
    mismatched.id = MessageId::new();
    assert!(store
        .complete_turn(turn_id, lease_token, 0, lease_expires_at, &mismatched)
        .await
        .is_err());
    assert_eq!(store.list_messages(chat.id).await.unwrap().len(), 2);
}

#[tokio::test]
async fn turn_failure_receipt_recovers_exact_retries_after_the_turn_advances() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    let accepted = match store
        .accept_turn(turn_id, chat.id, "gpt-5", "hello")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    entities::turn::Entity::update_many()
        .col_expr(
            entities::turn::Column::MaxAttempts,
            sea_orm::sea_query::Expr::value(2),
        )
        .filter(entities::turn::Column::Id.eq(turn_id.0))
        .exec(&store.conn)
        .await
        .unwrap();
    let claimed_at = accepted.available_at + chrono::Duration::seconds(1);
    let lease_expires_at = claimed_at + chrono::Duration::minutes(2);
    let token = uuid::Uuid::new_v4();
    store
        .claim_turn(token, claimed_at, lease_expires_at)
        .await
        .unwrap()
        .turn
        .unwrap();
    let resolved_at =
        claimed_at + chrono::Duration::seconds(1) + chrono::Duration::nanoseconds(999);
    let retry_at = resolved_at + chrono::Duration::minutes(1) + chrono::Duration::nanoseconds(777);
    let canonical_resolved_at =
        DateTime::<Utc>::from_timestamp_micros(resolved_at.timestamp_micros()).unwrap();
    let canonical_retry_at =
        DateTime::<Utc>::from_timestamp_micros(retry_at.timestamp_micros()).unwrap();
    let progress_steps = 2;
    let progress_usage = Usage {
        input_tokens: 13,
        output_tokens: 5,
        cache_read_input_tokens: 3,
        cache_creation_input_tokens: 2,
    };

    assert!(store
        .record_turn_failure(
            turn_id,
            token,
            resolved_at,
            TurnFailureRetry::RetryAt(canonical_resolved_at + chrono::Duration::nanoseconds(999)),
            0,
            Usage::default(),
            "provider_unavailable",
            None,
        )
        .await
        .is_err());
    assert_eq!(
        store
            .record_turn_failure(
                turn_id,
                uuid::Uuid::new_v4(),
                resolved_at,
                TurnFailureRetry::RetryAt(retry_at),
                0,
                Usage::default(),
                "provider_unavailable",
                Some("temporary outage"),
            )
            .await
            .unwrap(),
        None
    );

    let journaled = store
        .record_turn_failure_and_append_event(
            turn_id,
            token,
            resolved_at,
            TurnFailureRetry::RetryAt(retry_at),
            progress_steps,
            progress_usage,
            "provider_unavailable",
            Some("temporary outage"),
        )
        .await
        .unwrap()
        .unwrap();
    let RecordTurnFailureOutcome::Recorded(receipt) = journaled.outcome else {
        panic!("first exact failure must commit")
    };
    assert_eq!(journaled.terminal_event, None);
    assert!(store.list_events(chat.id, 0).await.unwrap().is_empty());
    assert_eq!(receipt.lease_token, token);
    assert_eq!(receipt.turn_id, turn_id);
    assert_eq!(receipt.attempt_count, 1);
    assert_eq!(receipt.requested_retry_at, Some(canonical_retry_at));
    assert_eq!(receipt.resolved_at, canonical_resolved_at);
    assert_eq!(receipt.result_status, TurnRunStatus::RetryWait);
    assert_eq!(receipt.model_steps, progress_steps);
    assert_eq!(receipt.usage, progress_usage);
    assert_eq!(receipt.error_code, "provider_unavailable");
    assert_eq!(receipt.error_detail.as_deref(), Some("temporary outage"));
    let waiting = store.get_turn(turn_id).await.unwrap().unwrap();
    assert_eq!(waiting.status, TurnRunStatus::RetryWait);
    assert_eq!(waiting.available_at, canonical_retry_at);
    assert_eq!(waiting.finished_at, None);
    assert_eq!(waiting.lease_token, None);
    assert_eq!(waiting.model_steps, progress_steps);
    assert_eq!(waiting.usage, progress_usage);
    assert_eq!(
        waiting.last_error_code.as_deref(),
        Some("provider_unavailable")
    );

    assert_eq!(
        store
            .record_turn_failure(
                turn_id,
                token,
                canonical_retry_at + chrono::Duration::hours(1),
                TurnFailureRetry::RetryAt(retry_at),
                progress_steps,
                progress_usage,
                "provider_unavailable",
                Some("temporary outage"),
            )
            .await
            .unwrap(),
        Some(RecordTurnFailureOutcome::Existing(receipt.clone()))
    );
    assert!(store
        .record_turn_failure_and_append_event(
            turn_id,
            token,
            resolved_at,
            TurnFailureRetry::Permanent,
            progress_steps,
            progress_usage,
            "provider_unavailable",
            Some("temporary outage"),
        )
        .await
        .is_err());
    assert!(store
        .record_turn_failure(
            turn_id,
            token,
            resolved_at,
            TurnFailureRetry::RetryAt(retry_at),
            progress_steps,
            progress_usage,
            "provider_unavailable",
            Some("different outage"),
        )
        .await
        .is_err());

    let second_token = uuid::Uuid::new_v4();
    let second_expiry = canonical_retry_at + chrono::Duration::minutes(2);
    let second = store
        .claim_turn(second_token, canonical_retry_at, second_expiry)
        .await
        .unwrap()
        .turn
        .unwrap();
    assert_eq!(second.attempt_count, 2);
    assert_eq!(second.last_error_code, None);
    assert_eq!(second.model_steps, progress_steps);
    assert_eq!(second.usage, progress_usage);
    let output = Message {
        id: MessageId::new(),
        chat_id: chat.id,
        turn_id,
        role: Role::Assistant,
        reasoning: Default::default(),
        content: "recovered".into(),
        llm_content: None,
        created_at: canonical_retry_at + chrono::Duration::seconds(1),
    };
    assert!(matches!(
        store
            .complete_turn(turn_id, second_token, 0, output.created_at, &output)
            .await
            .unwrap(),
        Some(CompleteTurnRunOutcome::Completed(_))
    ));
    assert_eq!(
        store
            .record_turn_failure(
                turn_id,
                token,
                second_expiry + chrono::Duration::hours(1),
                TurnFailureRetry::RetryAt(retry_at),
                progress_steps,
                progress_usage,
                "provider_unavailable",
                Some("temporary outage"),
            )
            .await
            .unwrap(),
        Some(RecordTurnFailureOutcome::Existing(receipt))
    );
}

/// CI-observed wedge: a worker that loses the failure-resolution race to the
/// lease scanner retries its exact request after the wall clock has passed its
/// requested retry time. That caller must learn the lease is no longer its to
/// resolve (`Ok(None)`), not receive the future-retry invariant error forever.
#[tokio::test]
async fn stale_lease_failure_with_passed_retry_time_reports_the_lost_race() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    let accepted = match store
        .accept_turn(turn_id, chat.id, "gpt-5", "hello")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    set_turn_max_attempts(&store, turn_id, 2).await;
    let claimed_at = accepted.available_at + chrono::Duration::seconds(1);
    let worker_token = uuid::Uuid::new_v4();
    let lease_expires_at = claimed_at + chrono::Duration::minutes(2);
    store
        .claim_turn(worker_token, claimed_at, lease_expires_at)
        .await
        .unwrap()
        .turn
        .unwrap();

    // The scanner claims past the worker's lease, which retries the turn on a
    // scan lease, then expires that lease too, terminalizing the turn out from
    // under the worker.
    let scan_at = lease_expires_at + chrono::Duration::microseconds(1);
    let retried = store
        .claim_turn(
            uuid::Uuid::new_v4(),
            scan_at,
            scan_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap()
        .turn
        .unwrap();
    let second_scan_at = retried.lease_expires_at.unwrap() + chrono::Duration::microseconds(1);
    let scanned = store
        .claim_turn(
            uuid::Uuid::new_v4(),
            second_scan_at,
            second_scan_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap();
    assert!(scanned.terminal_event.is_some());

    // The worker retries its exact failure request; its retry time is now in
    // the past relative to both the request and the database clock.
    assert_eq!(
        store
            .record_turn_failure(
                turn_id,
                worker_token,
                second_scan_at + chrono::Duration::seconds(1),
                TurnFailureRetry::RetryAt(claimed_at + chrono::Duration::seconds(2)),
                0,
                Usage::default(),
                "provider_unavailable",
                Some("temporary outage"),
            )
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn turn_failure_exhaustion_retains_retry_intent_and_rolls_back_atomically() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    let accepted = match store
        .accept_turn(turn_id, chat.id, "gpt-5", "hello")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    set_turn_max_attempts(&store, turn_id, 1).await;
    let claimed_at = accepted.available_at + chrono::Duration::seconds(1);
    let token = uuid::Uuid::new_v4();
    store
        .claim_turn(token, claimed_at, claimed_at + chrono::Duration::minutes(2))
        .await
        .unwrap()
        .turn
        .unwrap();
    let failed_at = claimed_at + chrono::Duration::seconds(1);
    let retry_at = failed_at + chrono::Duration::minutes(1);
    store
        .conn
        .execute_unprepared(
            "CREATE TRIGGER fail_turn_failure
             BEFORE UPDATE OF status ON \"turn\"
             WHEN NEW.status = 'failed'
             BEGIN SELECT RAISE(FAIL, 'forced turn failure rollback'); END",
        )
        .await
        .unwrap();
    assert!(store
        .record_turn_failure(
            turn_id,
            token,
            failed_at,
            TurnFailureRetry::RetryAt(retry_at),
            0,
            Usage::default(),
            "provider_error",
            None,
        )
        .await
        .is_err());
    assert!(entities::code_turn_failure::Entity::find_by_id(token)
        .one(&store.conn)
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        store.get_turn(turn_id).await.unwrap().unwrap().status,
        TurnRunStatus::Running
    );
    store
        .conn
        .execute_unprepared("DROP TRIGGER fail_turn_failure")
        .await
        .unwrap();

    let journaled = store
        .record_turn_failure_and_append_event(
            turn_id,
            token,
            failed_at,
            TurnFailureRetry::RetryAt(retry_at),
            0,
            Usage::default(),
            "provider_error",
            None,
        )
        .await
        .unwrap()
        .unwrap();
    let RecordTurnFailureOutcome::Recorded(receipt) = journaled.outcome else {
        panic!("failure after rollback must commit")
    };
    assert_eq!(receipt.result_status, TurnRunStatus::Failed);
    assert_eq!(receipt.requested_retry_at, Some(retry_at));
    let failed = store.get_turn(turn_id).await.unwrap().unwrap();
    assert_eq!(failed.status, TurnRunStatus::Failed);
    assert_eq!(failed.finished_at, Some(failed_at));
    assert_eq!(failed.available_at, accepted.available_at);
    assert_eq!(failed.last_error_code.as_deref(), Some("provider_error"));
    let terminal = journaled
        .terminal_event
        .expect("terminal failure must append an event");
    assert_eq!(
        terminal.event,
        AgentEvent::TurnFailed {
            error: crate::error::AgentErrorInfo {
                kind: "provider_error".into(),
                message: "provider_error".into(),
            }
        }
    );
    assert_eq!(store.list_events(chat.id, 0).await.unwrap(), vec![terminal]);
}

#[tokio::test]
async fn turn_failure_rolls_back_receipt_and_state_when_terminal_event_fails() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    let accepted = match store
        .accept_turn(turn_id, chat.id, "gpt-5", "hello")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    let claimed_at = accepted.available_at + chrono::Duration::seconds(1);
    let token = uuid::Uuid::new_v4();
    store
        .claim_turn(token, claimed_at, claimed_at + chrono::Duration::minutes(2))
        .await
        .unwrap()
        .turn
        .unwrap();
    entities::event::ActiveModel {
        session_id: Set(chat.id.0),
        owner: Set("local".to_owned()),
        seq: Set(1),
        turn_id: Set(Some(turn_id.0)),
        lease_token: Set(Some(token)),
        attempt_event_ordinal: Set(Some(i32::MAX)),
        scan_token: Set(None),
        terminal: Set(true),
        event: Set(serde_json::to_value(crate::chat_journal::journal_row(
            &AgentEvent::TurnCompleted {
                usage: Usage::default(),
                stop_reason: StopReason::EndTurn,
            },
        ))
        .unwrap()),
        created_at: Set(Utc::now()),
    }
    .insert(&store.conn)
    .await
    .unwrap();

    assert!(store
        .record_turn_failure_and_append_event(
            turn_id,
            token,
            claimed_at + chrono::Duration::seconds(1),
            TurnFailureRetry::Permanent,
            0,
            Usage::default(),
            "provider_error",
            Some("terminal insert must fail"),
        )
        .await
        .is_err());
    assert!(entities::code_turn_failure::Entity::find_by_id(token)
        .one(&store.conn)
        .await
        .unwrap()
        .is_none());
    let still_running = store.get_turn(turn_id).await.unwrap().unwrap();
    assert_eq!(still_running.status, TurnRunStatus::Running);
    assert_eq!(still_running.finished_at, None);
    assert_eq!(still_running.last_error_code, None);
    assert!(entities::code_turn_failure::Entity::find_by_id(token)
        .one(&store.conn)
        .await
        .unwrap()
        .is_none());
    assert_eq!(store.list_events(chat.id, 0).await.unwrap().len(), 1);
}

#[tokio::test]
async fn permanent_turn_failure_uses_the_heartbeated_lease_and_rejects_expiry() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    let accepted = match store
        .accept_turn(turn_id, chat.id, "gpt-5", "hello")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    let claimed_at = accepted.available_at + chrono::Duration::seconds(1);
    let original_expiry = claimed_at + chrono::Duration::minutes(1);
    let token = uuid::Uuid::new_v4();
    store
        .claim_turn(token, claimed_at, original_expiry)
        .await
        .unwrap()
        .turn
        .unwrap();
    let heartbeat_at = claimed_at + chrono::Duration::seconds(30);
    let extended_expiry = original_expiry + chrono::Duration::minutes(1);
    assert!(store
        .heartbeat_turn(turn_id, token, heartbeat_at, extended_expiry)
        .await
        .unwrap());
    let failed_at = original_expiry + chrono::Duration::seconds(1);
    let failure_usage = Usage {
        input_tokens: 7,
        output_tokens: 3,
        cache_read_input_tokens: 2,
        cache_creation_input_tokens: 1,
    };
    let RecordTurnFailureOutcome::Recorded(receipt) = store
        .record_turn_failure(
            turn_id,
            token,
            failed_at,
            TurnFailureRetry::Permanent,
            2,
            failure_usage,
            "unsafe_to_retry",
            Some("tool outcome is ambiguous"),
        )
        .await
        .unwrap()
        .unwrap()
    else {
        panic!("live heartbeated failure must commit")
    };
    assert_eq!(receipt.requested_retry_at, None);
    assert_eq!(receipt.resolved_at, failed_at);
    assert_eq!(receipt.result_status, TurnRunStatus::Failed);
    assert_eq!(receipt.model_steps, 2);
    assert_eq!(receipt.usage, failure_usage);
    let failed = store.get_turn(turn_id).await.unwrap().unwrap();
    assert_eq!(failed.status, TurnRunStatus::Failed);
    assert_eq!(failed.finished_at, Some(failed_at));
    assert_eq!(failed.last_error_code.as_deref(), Some("unsafe_to_retry"));
    assert_eq!(failed.model_steps, 2);
    assert_eq!(failed.usage, failure_usage);
    assert_eq!(
        store
            .record_turn_failure(
                turn_id,
                token,
                failed_at + chrono::Duration::hours(1),
                TurnFailureRetry::Permanent,
                2,
                failure_usage,
                "unsafe_to_retry",
                Some("tool outcome is ambiguous"),
            )
            .await
            .unwrap(),
        Some(RecordTurnFailureOutcome::Existing(receipt.clone()))
    );
    assert!(store
        .record_turn_failure(
            turn_id,
            token,
            failed_at + chrono::Duration::hours(1),
            TurnFailureRetry::Permanent,
            3,
            failure_usage,
            "unsafe_to_retry",
            Some("tool outcome is ambiguous"),
        )
        .await
        .is_err());

    let expired_chat = sample_chat();
    store.create_chat(&expired_chat).await.unwrap();
    let expired_turn = match store
        .accept_turn(TurnId::new(), expired_chat.id, "gpt-5", "expired")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    let expired_claim_at = expired_turn.available_at + chrono::Duration::seconds(1);
    let expired_at = expired_claim_at + chrono::Duration::minutes(1);
    let expired_token = uuid::Uuid::new_v4();
    store
        .claim_turn(expired_token, expired_claim_at, expired_at)
        .await
        .unwrap()
        .turn
        .unwrap();
    assert_eq!(
        store
            .record_turn_failure(
                expired_turn.id,
                expired_token,
                expired_at,
                TurnFailureRetry::Permanent,
                0,
                Usage::default(),
                "too_late",
                None,
            )
            .await
            .unwrap(),
        None
    );
    assert!(
        entities::code_turn_failure::Entity::find_by_id(expired_token)
            .one(&store.conn)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store
            .get_turn(expired_turn.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        TurnRunStatus::Running
    );
}

#[tokio::test]
async fn turn_completion_and_cancellation_serialize_to_one_decision() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn = match store
        .accept_turn(TurnId::new(), chat.id, "gpt-5", "race cancellation")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    let claimed_at = turn.available_at + chrono::Duration::seconds(1);
    let token = uuid::Uuid::new_v4();
    store
        .claim_turn(token, claimed_at, claimed_at + chrono::Duration::minutes(1))
        .await
        .unwrap()
        .turn
        .unwrap();
    let decided_at = claimed_at + chrono::Duration::seconds(1);
    let output = Message {
        id: MessageId::new(),
        chat_id: chat.id,
        turn_id: turn.id,
        role: Role::Assistant,
        reasoning: Default::default(),
        content: "race answer".into(),
        llm_content: None,
        created_at: decided_at,
    };
    let store = std::sync::Arc::new(store);
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    let completion = {
        let store = store.clone();
        let barrier = barrier.clone();
        tokio::spawn(async move {
            barrier.wait().await;
            store
                .complete_turn(turn.id, token, 0, decided_at, &output)
                .await
        })
    };
    let cancellation = {
        let store = store.clone();
        let barrier = barrier.clone();
        tokio::spawn(async move {
            barrier.wait().await;
            store.request_turn_cancellation(turn.id, decided_at).await
        })
    };
    let completion = completion.await.unwrap().unwrap();
    let cancellation = cancellation.await.unwrap().unwrap().unwrap();
    match (completion, cancellation) {
        (
            Some(CompleteTurnRunOutcome::Completed(completed)),
            RequestTurnCancellationOutcome::AlreadyTerminal(observed),
        ) => {
            assert_eq!(observed, completed);
            assert_eq!(store.list_messages(chat.id).await.unwrap().len(), 2);
        }
        (None, RequestTurnCancellationOutcome::Requested(cancelling)) => {
            assert_eq!(cancelling.status, TurnRunStatus::Cancelling);
            assert_eq!(store.list_messages(chat.id).await.unwrap().len(), 1);
        }
        outcomes => panic!("unexpected completion/cancellation race: {outcomes:?}"),
    }
}

#[tokio::test]
async fn turn_completion_and_failure_serialize_to_one_terminal_decision() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    let accepted = match store
        .accept_turn(turn_id, chat.id, "gpt-5", "hello")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    entities::turn::Entity::update_many()
        .col_expr(
            entities::turn::Column::MaxAttempts,
            sea_orm::sea_query::Expr::value(2),
        )
        .filter(entities::turn::Column::Id.eq(turn_id.0))
        .exec(&store.conn)
        .await
        .unwrap();
    let claimed_at = accepted.available_at + chrono::Duration::seconds(1);
    let token = uuid::Uuid::new_v4();
    store
        .claim_turn(token, claimed_at, claimed_at + chrono::Duration::minutes(2))
        .await
        .unwrap()
        .turn
        .unwrap();
    let resolved_at = claimed_at + chrono::Duration::seconds(1);
    let output = Message {
        id: MessageId::new(),
        chat_id: chat.id,
        turn_id,
        role: Role::Assistant,
        reasoning: Default::default(),
        content: "race winner".into(),
        llm_content: None,
        created_at: resolved_at,
    };
    let store = std::sync::Arc::new(store);
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    let completion = {
        let store = store.clone();
        let barrier = barrier.clone();
        let output = output.clone();
        tokio::spawn(async move {
            barrier.wait().await;
            store
                .complete_turn(turn_id, token, 0, resolved_at, &output)
                .await
        })
    };
    let failure = {
        let store = store.clone();
        let barrier = barrier.clone();
        tokio::spawn(async move {
            barrier.wait().await;
            store
                .record_turn_failure(
                    turn_id,
                    token,
                    resolved_at,
                    TurnFailureRetry::RetryAt(resolved_at + chrono::Duration::minutes(1)),
                    0,
                    Usage::default(),
                    "provider_error",
                    None,
                )
                .await
        })
    };
    let completion = completion.await.unwrap().unwrap();
    let failure = failure.await.unwrap().unwrap();
    let turn = store.get_turn(turn_id).await.unwrap().unwrap();
    match (completion, failure) {
        (Some(CompleteTurnRunOutcome::Completed(_)), None) => {
            assert_eq!(turn.status, TurnRunStatus::Completed);
            assert_eq!(store.list_messages(chat.id).await.unwrap().len(), 2);
            assert!(entities::code_turn_failure::Entity::find_by_id(token)
                .one(&store.conn)
                .await
                .unwrap()
                .is_none());
        }
        (None, Some(RecordTurnFailureOutcome::Recorded(receipt))) => {
            assert_eq!(receipt.result_status, TurnRunStatus::RetryWait);
            assert_eq!(turn.status, TurnRunStatus::RetryWait);
            assert_eq!(store.list_messages(chat.id).await.unwrap().len(), 1);
        }
        outcomes => panic!("unexpected completion/failure race: {outcomes:?}"),
    }
}

#[tokio::test]
async fn turn_completion_uses_the_heartbeated_lease_and_fences_operation_time() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    let accepted = match store
        .accept_turn(turn_id, chat.id, "gpt-5", "hello")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    let claimed_at = accepted.available_at + chrono::Duration::seconds(1);
    let original_expiry = claimed_at + chrono::Duration::minutes(1);
    let token = uuid::Uuid::new_v4();
    store
        .claim_turn(token, claimed_at, original_expiry)
        .await
        .unwrap()
        .turn
        .unwrap();
    let heartbeat_at = claimed_at + chrono::Duration::seconds(30);
    let extended_expiry = original_expiry + chrono::Duration::minutes(1);
    assert!(store
        .heartbeat_turn(turn_id, token, heartbeat_at, extended_expiry)
        .await
        .unwrap());

    let prepared_output = Message {
        id: MessageId::new(),
        chat_id: chat.id,
        turn_id,
        role: Role::Assistant,
        reasoning: Default::default(),
        content: "prepared before retry".into(),
        llm_content: None,
        created_at: heartbeat_at - chrono::Duration::microseconds(1),
    };
    let future_output = Message {
        id: MessageId::new(),
        content: "future output".into(),
        llm_content: None,
        created_at: heartbeat_at + chrono::Duration::seconds(2),
        ..prepared_output.clone()
    };
    assert!(store
        .complete_turn(
            turn_id,
            token,
            0,
            heartbeat_at + chrono::Duration::seconds(1),
            &future_output,
        )
        .await
        .is_err());
    let output = Message {
        id: MessageId::new(),
        content: "after the original lease".into(),
        llm_content: None,
        created_at: original_expiry + chrono::Duration::seconds(1),
        ..prepared_output
    };
    assert!(matches!(
        store
            .complete_turn(turn_id, token, 0, output.created_at, &output)
            .await
            .unwrap(),
        Some(CompleteTurnRunOutcome::Completed(_))
    ));
    assert_eq!(store.list_messages(chat.id).await.unwrap().len(), 2);
}

#[tokio::test]
async fn turn_completion_rejects_prepared_output_retried_after_expiry() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    let accepted = match store
        .accept_turn(turn_id, chat.id, "gpt-5", "hello")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    let claimed_at = accepted.available_at + chrono::Duration::seconds(1);
    let lease_expires_at = claimed_at + chrono::Duration::minutes(1);
    let token = uuid::Uuid::new_v4();
    store
        .claim_turn(token, claimed_at, lease_expires_at)
        .await
        .unwrap()
        .turn
        .unwrap();
    let prepared_output = Message {
        id: MessageId::new(),
        chat_id: chat.id,
        turn_id,
        role: Role::Assistant,
        reasoning: Default::default(),
        content: "prepared while live".into(),
        llm_content: None,
        created_at: lease_expires_at - chrono::Duration::seconds(1),
    };

    assert_eq!(
        store
            .complete_turn(turn_id, token, 0, lease_expires_at, &prepared_output)
            .await
            .unwrap(),
        None
    );
    assert_eq!(store.list_messages(chat.id).await.unwrap().len(), 1);
    assert_eq!(
        store.get_turn(turn_id).await.unwrap().unwrap().status,
        TurnRunStatus::Running
    );
}

#[tokio::test]
async fn concurrent_different_turn_completions_commit_one_output_once() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    let accepted = match store
        .accept_turn(turn_id, chat.id, "gpt-5", "hello")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    let claimed_at = accepted.available_at + chrono::Duration::seconds(1);
    let token = uuid::Uuid::new_v4();
    store
        .claim_turn(token, claimed_at, claimed_at + chrono::Duration::minutes(1))
        .await
        .unwrap()
        .turn
        .unwrap();
    let output = Message {
        id: MessageId::new(),
        chat_id: chat.id,
        turn_id,
        role: Role::Assistant,
        reasoning: Default::default(),
        content: "one answer".into(),
        llm_content: None,
        created_at: claimed_at + chrono::Duration::seconds(1),
    };
    let competing_output = Message {
        id: MessageId::new(),
        content: "different answer".into(),
        llm_content: None,
        ..output.clone()
    };
    let store = std::sync::Arc::new(store);
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    let mut tasks = Vec::new();
    for output in [output, competing_output] {
        let store = store.clone();
        let barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            store
                .complete_turn(turn_id, token, 0, output.created_at, &output)
                .await
        }));
    }
    let mut committed = 0;
    let mut conflicted = 0;
    for task in tasks {
        match task.await.unwrap() {
            Ok(Some(CompleteTurnRunOutcome::Completed(_))) => committed += 1,
            Err(AgentError::Store(message))
                if message.contains("already completed with different output") =>
            {
                conflicted += 1;
            }
            outcome => panic!("unexpected concurrent completion outcome: {outcome:?}"),
        }
    }
    assert_eq!((committed, conflicted), (1, 1));
    assert_eq!(store.list_messages(chat.id).await.unwrap().len(), 2);
}

#[tokio::test]
async fn turn_completion_rolls_back_output_when_state_update_fails() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    let accepted = match store
        .accept_turn(turn_id, chat.id, "gpt-5", "hello")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    let claimed_at = accepted.available_at + chrono::Duration::seconds(1);
    let token = uuid::Uuid::new_v4();
    store
        .claim_turn(token, claimed_at, claimed_at + chrono::Duration::minutes(1))
        .await
        .unwrap()
        .turn
        .unwrap();
    store
        .conn
        .execute_unprepared(
            "CREATE TRIGGER fail_turn_completion
             BEFORE UPDATE OF status ON \"turn\"
             WHEN NEW.status = 'completed'
             BEGIN SELECT RAISE(FAIL, 'forced turn completion failure'); END",
        )
        .await
        .unwrap();
    let output = Message {
        id: MessageId::new(),
        chat_id: chat.id,
        turn_id,
        role: Role::Assistant,
        reasoning: Default::default(),
        content: "must roll back".into(),
        llm_content: None,
        created_at: claimed_at + chrono::Duration::seconds(1),
    };
    assert!(store
        .complete_turn(turn_id, token, 0, output.created_at, &output)
        .await
        .is_err());
    assert_eq!(store.list_messages(chat.id).await.unwrap().len(), 1);
    let still_running = store.get_turn(turn_id).await.unwrap().unwrap();
    assert_eq!(still_running.status, TurnRunStatus::Running);
    assert_eq!(still_running.output_message_id, None);
}

#[tokio::test]
async fn turn_completion_rolls_back_state_and_output_when_terminal_event_fails() {
    use crate::error::AgentErrorInfo;
    use crate::event::AgentEvent;
    use crate::provider::{StopReason, Usage};

    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    let accepted = match store
        .accept_turn(turn_id, chat.id, "gpt-5", "hello")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    let claimed_at = accepted.available_at + chrono::Duration::seconds(1);
    let token = uuid::Uuid::new_v4();
    store
        .claim_turn(token, claimed_at, claimed_at + chrono::Duration::minutes(1))
        .await
        .unwrap()
        .turn
        .unwrap();
    entities::event::ActiveModel {
        session_id: Set(chat.id.0),
        owner: Set("local".to_owned()),
        seq: Set(1),
        turn_id: Set(Some(turn_id.0)),
        lease_token: Set(Some(token)),
        attempt_event_ordinal: Set(Some(i32::MAX)),
        scan_token: Set(None),
        terminal: Set(true),
        event: Set(serde_json::to_value(crate::chat_journal::journal_row(
            &AgentEvent::TurnFailed {
                error: AgentErrorInfo {
                    kind: "forced".into(),
                    message: "occupy terminal slot".into(),
                },
            },
        ))
        .unwrap()),
        created_at: Set(Utc::now()),
    }
    .insert(&store.conn)
    .await
    .unwrap();
    let output = Message {
        id: MessageId::new(),
        chat_id: chat.id,
        turn_id,
        role: Role::Assistant,
        reasoning: Default::default(),
        content: "must roll back".into(),
        llm_content: None,
        created_at: claimed_at + chrono::Duration::seconds(1),
    };

    assert!(store
        .complete_turn_and_append_event(
            turn_id,
            token,
            0,
            output.created_at,
            &output,
            0,
            Usage::default(),
            StopReason::EndTurn,
        )
        .await
        .is_err());
    assert_eq!(store.list_messages(chat.id).await.unwrap().len(), 1);
    let still_running = store.get_turn(turn_id).await.unwrap().unwrap();
    assert_eq!(still_running.status, TurnRunStatus::Running);
    assert_eq!(still_running.output_message_id, None);
    assert_eq!(store.list_events(chat.id, 0).await.unwrap().len(), 1);
}

#[tokio::test]
async fn refused_turn_metadata_hydrates_with_its_exact_durable_output() {
    use crate::event::AgentEvent;
    use crate::provider::{RefusalDetails, RefusalOutcome, Usage};

    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();

    let mut expected = Vec::new();
    for (content, category, partial_output) in [
        ("", "cyber", false),
        ("Visible partial", "general_harms", true),
    ] {
        let turn_id = TurnId::new();
        let accepted = match store
            .accept_turn(turn_id, chat.id, "claude", "question")
            .await
            .unwrap()
        {
            AcceptTurnOutcome::Accepted(turn) => turn,
            outcome => panic!("unexpected acceptance: {outcome:?}"),
        };
        let lease_token = uuid::Uuid::new_v4();
        store
            .claim_turn(
                lease_token,
                accepted.available_at,
                accepted.available_at + chrono::Duration::minutes(1),
            )
            .await
            .unwrap()
            .turn
            .expect("accepted turn is claimable");
        let output = Message {
            id: MessageId::new(),
            chat_id: chat.id,
            turn_id,
            role: Role::Assistant,
            reasoning: Default::default(),
            content: content.into(),
            llm_content: None,
            created_at: accepted.available_at,
        };
        let refusal = RefusalOutcome::new(
            RefusalDetails::from_category(Some(category)),
            partial_output,
        );
        let completed = store
            .complete_refused_turn_with_citations_and_append_event(
                turn_id,
                lease_token,
                0,
                output.created_at,
                &output,
                &[],
                0,
                Usage::default(),
                refusal.clone(),
            )
            .await
            .unwrap()
            .expect("live refusal completion");
        assert!(matches!(
            completed.terminal_event.as_ref().map(|event| &event.event),
            Some(AgentEvent::TurnRefused {
                refusal: journaled,
                ..
            }) if journaled == &refusal
        ));
        expected.push((output, refusal));
    }

    let transcript = store.get_chat_transcript(chat.id).await.unwrap().unwrap();
    assert_eq!(transcript.terminal_turns.len(), 2);
    for (output, refusal) in expected {
        let stored = transcript
            .messages
            .iter()
            .find(|message| message.id == output.id)
            .expect("refused output remains a normal durable assistant message");
        assert_eq!(stored.content, output.content);
        assert!(transcript.terminal_turns.iter().any(|snapshot| {
            snapshot.message_id == Some(output.id)
                && snapshot.status == crate::storage::ChatTerminalTurnStatus::Completed
                && snapshot.refusal.as_ref() == Some(&refusal)
        }));
    }
}
