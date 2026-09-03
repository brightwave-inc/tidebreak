use super::*;

#[tokio::test]
async fn queued_and_retry_wait_turns_cancel_immediately_and_idempotently() {
    let (_dir, store) = temp_store().await;
    let queued_chat = sample_chat();
    store.create_chat(&queued_chat).await.unwrap();
    let queued = match store
        .accept_turn(TurnId::new(), queued_chat.id, "gpt-5", "queued")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    assert_eq!(
        store
            .request_turn_cancellation_and_append_event(
                queued.id,
                queued.updated_at - chrono::Duration::seconds(1),
            )
            .await
            .unwrap(),
        None
    );
    let cancelled_at = queued.updated_at + chrono::Duration::seconds(1);
    let journaled = store
        .request_turn_cancellation_and_append_event(queued.id, cancelled_at)
        .await
        .unwrap()
        .unwrap();
    let RequestTurnCancellationOutcome::Cancelled(cancelled) = journaled.outcome else {
        panic!("queued cancellation must be immediate")
    };
    assert_eq!(cancelled.status, TurnRunStatus::Cancelled);
    assert_eq!(cancelled.attempt_count, 0);
    assert_eq!(cancelled.finished_at, Some(cancelled_at));
    let terminal = journaled
        .terminal_event
        .expect("queued cancellation must append a terminal event");
    assert_eq!(
        terminal.event,
        AgentEvent::TurnCancelled {
            usage: Usage::default()
        }
    );
    let recovered = store
        .request_turn_cancellation_and_append_event(queued.id, queued.updated_at)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        recovered.outcome,
        RequestTurnCancellationOutcome::Existing(cancelled)
    );
    assert_eq!(recovered.terminal_event, Some(terminal));
    let after_cancel = match store
        .accept_turn(TurnId::new(), queued_chat.id, "gpt-5", "after cancel")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    assert!(matches!(
        store
            .request_turn_cancellation(
                after_cancel.id,
                after_cancel.updated_at + chrono::Duration::seconds(1),
            )
            .await
            .unwrap(),
        Some(RequestTurnCancellationOutcome::Cancelled(_))
    ));

    let retry_chat = sample_chat();
    store.create_chat(&retry_chat).await.unwrap();
    let retry_turn = match store
        .accept_turn(TurnId::new(), retry_chat.id, "gpt-5", "retry")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    entities::code_turn::Entity::update_many()
        .col_expr(
            entities::code_turn::Column::MaxAttempts,
            sea_orm::sea_query::Expr::value(2),
        )
        .filter(entities::code_turn::Column::Id.eq(retry_turn.id.0))
        .exec(&store.conn)
        .await
        .unwrap();
    let claimed_at = retry_turn.available_at + chrono::Duration::seconds(1);
    let token = uuid::Uuid::new_v4();
    store
        .claim_turn(token, claimed_at, claimed_at + chrono::Duration::minutes(2))
        .await
        .unwrap()
        .turn
        .unwrap();
    let failed_at = claimed_at + chrono::Duration::seconds(1);
    let retry_at = failed_at + chrono::Duration::minutes(1);
    let RecordTurnFailureOutcome::Recorded(receipt) = store
        .record_turn_failure(
            retry_turn.id,
            token,
            failed_at,
            TurnFailureRetry::RetryAt(retry_at),
            0,
            Usage::default(),
            "provider_unavailable",
            Some("temporary outage"),
        )
        .await
        .unwrap()
        .unwrap()
    else {
        panic!("retryable failure must commit")
    };
    let retry_cancelled_at = failed_at + chrono::Duration::seconds(1);
    let journaled = store
        .request_turn_cancellation_and_append_event(retry_turn.id, retry_cancelled_at)
        .await
        .unwrap()
        .unwrap();
    let RequestTurnCancellationOutcome::Cancelled(cancelled) = journaled.outcome else {
        panic!("retry-wait cancellation must be immediate")
    };
    assert!(matches!(
        journaled.terminal_event,
        Some(SequencedEvent {
            event: AgentEvent::TurnCancelled { .. },
            ..
        })
    ));
    assert_eq!(cancelled.status, TurnRunStatus::Cancelled);
    assert_eq!(cancelled.finished_at, Some(retry_cancelled_at));
    assert_eq!(cancelled.last_error_code, None);
    assert_eq!(cancelled.last_error_detail, None);
    assert_eq!(
        store
            .record_turn_failure(
                retry_turn.id,
                token,
                retry_cancelled_at,
                TurnFailureRetry::RetryAt(retry_at),
                0,
                Usage::default(),
                "provider_unavailable",
                Some("temporary outage"),
            )
            .await
            .unwrap(),
        Some(RecordTurnFailureOutcome::Existing(receipt))
    );
}

#[tokio::test]
async fn immediate_cancellation_rolls_back_when_terminal_event_fails() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn = match store
        .accept_turn(TurnId::new(), chat.id, "gpt-5", "cancel before claim")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    entities::code_event::ActiveModel {
        session_id: Set(chat.id.0),
        owner: Set("local".to_owned()),
        seq: Set(1),
        turn_id: Set(Some(turn.id.0)),
        lease_token: Set(None),
        attempt_event_ordinal: Set(None),
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
        .request_turn_cancellation_and_append_event(
            turn.id,
            turn.updated_at + chrono::Duration::seconds(1),
        )
        .await
        .is_err());
    let still_queued = store.get_turn(turn.id).await.unwrap().unwrap();
    assert_eq!(still_queued.status, TurnRunStatus::Queued);
    assert_eq!(still_queued.finished_at, None);
    assert_eq!(store.list_events(chat.id, 0).await.unwrap().len(), 1);
}

#[tokio::test]
async fn running_turn_cancellation_holds_the_chat_until_exact_worker_acknowledgement() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn = match store
        .accept_turn(TurnId::new(), chat.id, "gpt-5", "running")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    let claimed_at = turn.available_at + chrono::Duration::seconds(1);
    let expires_at = claimed_at + chrono::Duration::minutes(1);
    let token = uuid::Uuid::new_v4();
    store
        .claim_turn(token, claimed_at, expires_at)
        .await
        .unwrap()
        .turn
        .unwrap();
    let requested_at = claimed_at + chrono::Duration::seconds(1);
    let requested = store
        .request_turn_cancellation_and_append_event(turn.id, requested_at)
        .await
        .unwrap()
        .unwrap();
    let RequestTurnCancellationOutcome::Requested(cancelling) = requested.outcome else {
        panic!("running cancellation must await worker acknowledgement")
    };
    assert_eq!(requested.terminal_event, None);
    assert_eq!(cancelling.status, TurnRunStatus::Cancelling);
    assert_eq!(cancelling.lease_token, Some(token));
    assert_eq!(cancelling.lease_expires_at, Some(expires_at));
    assert_eq!(cancelling.finished_at, None);
    assert!(!store
        .heartbeat_turn(
            turn.id,
            token,
            requested_at + chrono::Duration::seconds(1),
            expires_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap());
    let output = Message {
        id: MessageId::new(),
        chat_id: chat.id,
        turn_id: turn.id,
        role: Role::Assistant,
        reasoning: Default::default(),
        content: "too late".into(),
        llm_content: None,
        created_at: requested_at + chrono::Duration::seconds(1),
    };
    assert_eq!(
        store
            .complete_turn(turn.id, token, 0, output.created_at, &output)
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        store
            .record_turn_failure(
                turn.id,
                token,
                output.created_at,
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
    assert!(matches!(
        store
            .accept_turn(TurnId::new(), chat.id, "gpt-5", "must remain busy")
            .await
            .unwrap(),
        AcceptTurnOutcome::ChatBusy(active) if active.status == TurnRunStatus::Cancelling
    ));
    assert_eq!(
        store
            .request_turn_cancellation(turn.id, claimed_at)
            .await
            .unwrap(),
        Some(RequestTurnCancellationOutcome::Existing(cancelling))
    );
    assert!(store
        .finish_turn_cancellation(turn.id, uuid::Uuid::nil(), requested_at)
        .await
        .is_err());
    assert_eq!(
        store
            .finish_turn_cancellation(turn.id, uuid::Uuid::new_v4(), requested_at)
            .await
            .unwrap(),
        None
    );

    let acknowledged_at = expires_at + chrono::Duration::seconds(1);
    let usage = Usage {
        input_tokens: 13,
        output_tokens: 8,
        ..Usage::default()
    };
    let final_model_steps = 2;
    let journaled = store
        .finish_turn_cancellation_and_append_event(
            turn.id,
            token,
            acknowledged_at,
            final_model_steps,
            usage,
            None,
            &[],
        )
        .await
        .unwrap()
        .unwrap();
    let FinishTurnCancellationOutcome::Cancelled(cancelled) = journaled.outcome else {
        panic!("exact worker acknowledgement must cancel")
    };
    assert_eq!(cancelled.status, TurnRunStatus::Cancelled);
    assert_eq!(cancelled.lease_token, None);
    assert_eq!(cancelled.lease_expires_at, None);
    assert_eq!(cancelled.finished_at, Some(acknowledged_at));
    assert_eq!(cancelled.model_steps, final_model_steps);
    let terminal = journaled
        .terminal_event
        .expect("worker acknowledgement must append a terminal event");
    assert_eq!(terminal.event, AgentEvent::TurnCancelled { usage });
    let recovered = store
        .finish_turn_cancellation_and_append_event(
            turn.id,
            token,
            acknowledged_at + chrono::Duration::hours(1),
            final_model_steps,
            usage,
            None,
            &[],
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        recovered.outcome,
        FinishTurnCancellationOutcome::Existing(cancelled)
    );
    assert_eq!(recovered.terminal_event, Some(terminal));
    assert!(store
        .finish_turn_cancellation_and_append_event(
            turn.id,
            token,
            acknowledged_at + chrono::Duration::hours(1),
            final_model_steps + 1,
            usage,
            None,
            &[],
        )
        .await
        .is_err());
    assert!(store
        .finish_turn_cancellation_and_append_event(
            turn.id,
            token,
            acknowledged_at + chrono::Duration::hours(1),
            final_model_steps,
            Usage::default(),
            None,
            &[],
        )
        .await
        .is_err());
    assert!(matches!(
        store
            .accept_turn(TurnId::new(), chat.id, "gpt-5", "after acknowledgement")
            .await
            .unwrap(),
        AcceptTurnOutcome::Accepted(_)
    ));
    assert_eq!(store.list_messages(chat.id).await.unwrap().len(), 2);
    // The stopped turn commits no assistant message, so the transcript carries
    // the terminal turn itself. Without it a reopened conversation cannot tell
    // a response that was stopped from one that finished.
    let transcript = store.get_chat_transcript(chat.id).await.unwrap().unwrap();
    assert_eq!(
        transcript.terminal_turns,
        vec![crate::storage::ChatTerminalTurnSnapshot {
            turn_id: turn.id,
            message_id: None,
            status: crate::storage::ChatTerminalTurnStatus::Cancelled,
            partial_content: String::new(),
            reasoning: String::new(),
            refusal: None,
            failure_kind: None,
            failure_detail: None,
            model: "gpt-5".into(),
            invoked_skills: Vec::new(),
            usage,
            voice_input_used: false,
            finished_at: acknowledged_at,
        }]
    );
}

#[tokio::test]
async fn cancellation_acknowledgement_accepts_the_request_callers_clock() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn = match store
        .accept_turn(TurnId::new(), chat.id, "gpt-5", "cancel immediately")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    let token = uuid::Uuid::new_v4();
    let claimed_at = Utc::now();
    store
        .claim_turn(token, claimed_at, claimed_at + chrono::Duration::minutes(1))
        .await
        .unwrap()
        .turn
        .expect("the queued turn is claimable");

    // Cancellation stamps its transition from the authoritative database
    // clock. That clock may be later than the caller's timestamp even though
    // both operations happen in the same application tick (SQLite explicitly
    // rounds it to the end of the current millisecond). The exact worker must
    // still be able to acknowledge with the timestamp it already captured.
    let caller_now = Utc::now();
    assert!(matches!(
        store
            .request_turn_cancellation(turn.id, caller_now)
            .await
            .unwrap(),
        Some(RequestTurnCancellationOutcome::Requested(_))
    ));
    assert!(matches!(
        store
            .finish_turn_cancellation(turn.id, token, caller_now)
            .await
            .unwrap(),
        Some(FinishTurnCancellationOutcome::Cancelled(_))
    ));
}

#[tokio::test]
async fn concurrent_cancellation_requests_converge_on_one_running_turn() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn = match store
        .accept_turn(TurnId::new(), chat.id, "gpt-5", "cancel concurrently")
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
    let store = std::sync::Arc::new(store);
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(8));
    let requested_at = claimed_at + chrono::Duration::seconds(1);
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let store = store.clone();
        let barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            store.request_turn_cancellation(turn.id, requested_at).await
        }));
    }
    let mut requested = 0;
    let mut existing = 0;
    for task in tasks {
        match task.await.unwrap().unwrap().unwrap() {
            RequestTurnCancellationOutcome::Requested(cancelling) => {
                requested += 1;
                assert_eq!(cancelling.lease_token, Some(token));
            }
            RequestTurnCancellationOutcome::Existing(cancelling) => {
                existing += 1;
                assert_eq!(cancelling.status, TurnRunStatus::Cancelling);
                assert_eq!(cancelling.lease_token, Some(token));
            }
            outcome => panic!("unexpected concurrent cancellation outcome: {outcome:?}"),
        }
    }
    assert_eq!((requested, existing), (1, 7));
    assert_eq!(
        store.get_turn(turn.id).await.unwrap().unwrap().status,
        TurnRunStatus::Cancelling
    );
}
