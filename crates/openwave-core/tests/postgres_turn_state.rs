#![cfg(feature = "postgres")]

use std::path::PathBuf;
use std::sync::Arc;

use chrono::{Duration, Utc};
use openwave_core::{
    AcceptTurnOutcome, AgentEvent, Chat, ChatId, CompleteTurnRunOutcome, DbStore,
    FinishTurnCancellationOutcome, Message, MessageId, RecordTurnFailureOutcome,
    RequestTurnCancellationOutcome, Role, StopReason, Store, TurnFailureRetry, TurnId,
    TurnRunStatus, Usage,
};

fn sample_chat() -> Chat {
    Chat {
        id: ChatId::new(),
        project_id: None,
        title: None,
        model: None,
        workspace_dir: PathBuf::from("/tmp/openwave-postgres-test"),
        created_at: Utc::now(),
    }
}

#[tokio::test]
async fn postgres_turn_acceptance_claims_and_receipts_are_atomic() {
    let url = match std::env::var("OPENWAVE_POSTGRES_TEST_URL") {
        Ok(url) => url,
        Err(_) if std::env::var_os("OPENWAVE_REQUIRE_POSTGRES_TEST").is_some() => {
            panic!("OPENWAVE_POSTGRES_TEST_URL must name an isolated test database")
        }
        Err(_) => return,
    };
    let store = Arc::new(DbStore::connect(&url).await.unwrap());
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    let accepted = match store
        .accept_turn(turn_id, chat.id, "gpt-5", "postgres input")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    assert!(matches!(
        store
            .accept_turn(turn_id, chat.id, "gpt-5", "postgres input")
            .await
            .unwrap(),
        AcceptTurnOutcome::Existing(_)
    ));

    let claim_at = accepted.available_at + Duration::seconds(1);
    let lease_expires_at = claim_at + Duration::minutes(1);
    let barrier = Arc::new(tokio::sync::Barrier::new(8));
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let store = store.clone();
        let barrier = barrier.clone();
        let token = uuid::Uuid::new_v4();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            (
                token,
                store
                    .claim_turn_run(token, claim_at, lease_expires_at)
                    .await,
            )
        }));
    }

    let mut winner = None;
    let mut empty = 0;
    for task in tasks {
        let (token, outcome) = task.await.unwrap();
        match outcome.unwrap().turn {
            Some(turn) => {
                assert!(winner.replace((token, turn)).is_none());
            }
            None => empty += 1,
        }
    }
    assert_eq!(empty, 7);
    let (token, claimed) = winner.unwrap();
    assert_eq!(claimed.id, turn_id);
    assert_eq!(claimed.status, TurnRunStatus::Running);
    assert_eq!(claimed.lease_token, Some(token));
    assert_eq!(
        store
            .claim_turn_run(token, claim_at, lease_expires_at)
            .await
            .unwrap()
            .turn,
        Some(claimed)
    );

    let expired = store
        .claim_turn_run(
            uuid::Uuid::new_v4(),
            lease_expires_at,
            lease_expires_at + Duration::minutes(1),
        )
        .await
        .unwrap();
    assert_eq!(expired.turn, None);
    let terminal = expired
        .terminal_event
        .expect("expired attempt must publish a terminal event");
    assert_eq!(terminal.chat_id, chat.id);
    assert_eq!(terminal.turn_id, turn_id);
    assert!(matches!(
        &terminal.event.event,
        AgentEvent::TurnFailed { error }
            if error.kind == "lease_expired"
                && error.message == "final worker lease expired"
    ));
    assert_eq!(
        store.list_events(chat.id, 0).await.unwrap(),
        vec![terminal.event]
    );
    assert_eq!(
        store.get_turn_run(turn_id).await.unwrap().unwrap().status,
        TurnRunStatus::Failed
    );

    let next_chat = sample_chat();
    store.create_chat(&next_chat).await.unwrap();
    let next = match store
        .accept_turn(TurnId::new(), next_chat.id, "gpt-5", "later input")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    let delayed_retry_at = next.available_at + Duration::seconds(1);
    assert_eq!(
        store
            .claim_turn_run(
                token,
                delayed_retry_at,
                delayed_retry_at + Duration::minutes(1),
            )
            .await
            .unwrap()
            .turn,
        None
    );
    assert_eq!(
        store.get_turn_run(next.id).await.unwrap().unwrap().status,
        TurnRunStatus::Queued
    );

    let completion_token = uuid::Uuid::new_v4();
    let completion_expiry = delayed_retry_at + Duration::minutes(1);
    store
        .claim_turn_run(completion_token, delayed_retry_at, completion_expiry)
        .await
        .unwrap()
        .turn
        .unwrap();
    let output = Message {
        id: MessageId::new(),
        chat_id: next_chat.id,
        turn_id: next.id,
        role: Role::Assistant,
        content: "postgres completion".into(),
        created_at: delayed_retry_at + Duration::seconds(1),
    };
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let mut completions = Vec::new();
    for _ in 0..2 {
        let store = store.clone();
        let barrier = barrier.clone();
        let output = output.clone();
        completions.push(tokio::spawn(async move {
            barrier.wait().await;
            store
                .complete_turn_run_and_append_event(
                    next.id,
                    completion_token,
                    output.created_at,
                    &output,
                    Usage::default(),
                    StopReason::EndTurn,
                )
                .await
                .unwrap()
                .unwrap()
        }));
    }
    let mut committed = 0;
    let mut existing = 0;
    let mut terminal_events = Vec::new();
    for completion in completions {
        let completion = completion.await.unwrap();
        match completion.outcome {
            CompleteTurnRunOutcome::Completed(_) => committed += 1,
            CompleteTurnRunOutcome::Existing(_) => existing += 1,
        }
        terminal_events.push(completion.terminal_event.unwrap());
    }
    assert_eq!((committed, existing), (1, 1));
    assert_eq!(terminal_events[0], terminal_events[1]);
    assert_eq!(terminal_events[0].seq, 1);
    assert_eq!(store.list_messages(next_chat.id).await.unwrap().len(), 2);

    let failure_chat = sample_chat();
    store.create_chat(&failure_chat).await.unwrap();
    let failure_turn = match store
        .accept_turn(TurnId::new(), failure_chat.id, "gpt-5", "postgres failure")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    let failure_token = uuid::Uuid::new_v4();
    let failure_claimed_at = failure_turn.available_at + Duration::seconds(1);
    let failure_at = failure_claimed_at + Duration::seconds(1);
    let requested_retry_at = failure_at + Duration::minutes(1);
    store
        .claim_turn_run(
            failure_token,
            failure_claimed_at,
            failure_claimed_at + Duration::minutes(2),
        )
        .await
        .unwrap()
        .turn
        .unwrap();
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let mut failures = Vec::new();
    for _ in 0..2 {
        let store = store.clone();
        let barrier = barrier.clone();
        failures.push(tokio::spawn(async move {
            barrier.wait().await;
            store
                .record_turn_run_failure_and_append_event(
                    failure_turn.id,
                    failure_token,
                    failure_at,
                    TurnFailureRetry::RetryAt(requested_retry_at),
                    "provider_unavailable",
                    Some("temporary outage"),
                )
                .await
                .unwrap()
                .unwrap()
        }));
    }
    let mut recorded = 0;
    let mut failure_existing = 0;
    let mut failure_events = Vec::new();
    for failure in failures {
        let failure = failure.await.unwrap();
        match failure.outcome {
            RecordTurnFailureOutcome::Recorded(receipt) => {
                recorded += 1;
                assert_eq!(receipt.result_status, TurnRunStatus::Failed);
                assert_eq!(receipt.requested_retry_at, Some(requested_retry_at));
            }
            RecordTurnFailureOutcome::Existing(receipt) => {
                failure_existing += 1;
                assert_eq!(receipt.result_status, TurnRunStatus::Failed);
                assert_eq!(receipt.requested_retry_at, Some(requested_retry_at));
            }
        }
        failure_events.push(failure.terminal_event.unwrap());
    }
    assert_eq!((recorded, failure_existing), (1, 1));
    assert_eq!(failure_events[0], failure_events[1]);
    assert_eq!(failure_events[0].seq, 1);
    assert_eq!(
        store
            .get_turn_run(failure_turn.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        TurnRunStatus::Failed
    );

    let cancellation_chat = sample_chat();
    store.create_chat(&cancellation_chat).await.unwrap();
    let cancellation_turn = match store
        .accept_turn(
            TurnId::new(),
            cancellation_chat.id,
            "gpt-5",
            "postgres cancellation",
        )
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    let cancellation_token = uuid::Uuid::new_v4();
    let cancellation_claimed_at = cancellation_turn.available_at + Duration::seconds(1);
    let cancellation_expiry = cancellation_claimed_at + Duration::minutes(1);
    let cancellation_requested_at = cancellation_claimed_at + Duration::seconds(1);
    store
        .claim_turn_run(
            cancellation_token,
            cancellation_claimed_at,
            cancellation_expiry,
        )
        .await
        .unwrap()
        .turn
        .unwrap();
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let mut cancellations = Vec::new();
    for _ in 0..2 {
        let store = store.clone();
        let barrier = barrier.clone();
        cancellations.push(tokio::spawn(async move {
            barrier.wait().await;
            store
                .request_turn_cancellation_and_append_event(
                    cancellation_turn.id,
                    cancellation_requested_at,
                )
                .await
                .unwrap()
                .unwrap()
        }));
    }
    let mut requested = 0;
    let mut cancellation_existing = 0;
    for cancellation in cancellations {
        let cancellation = cancellation.await.unwrap();
        assert_eq!(cancellation.terminal_event, None);
        match cancellation.outcome {
            RequestTurnCancellationOutcome::Requested(turn) => {
                requested += 1;
                assert_eq!(turn.status, TurnRunStatus::Cancelling);
            }
            RequestTurnCancellationOutcome::Existing(turn) => {
                cancellation_existing += 1;
                assert_eq!(turn.status, TurnRunStatus::Cancelling);
            }
            outcome => panic!("unexpected cancellation outcome: {outcome:?}"),
        }
    }
    assert_eq!((requested, cancellation_existing), (1, 1));
    let acknowledged_at = cancellation_expiry + Duration::seconds(1);
    let usage = Usage {
        input_tokens: 3,
        output_tokens: 2,
        ..Usage::default()
    };
    let journaled = store
        .finish_turn_cancellation_and_append_event(
            cancellation_turn.id,
            cancellation_token,
            acknowledged_at,
            usage,
        )
        .await
        .unwrap()
        .unwrap();
    let FinishTurnCancellationOutcome::Cancelled(cancelled) = journaled.outcome else {
        panic!("exact PostgreSQL cancellation acknowledgement must commit")
    };
    assert_eq!(cancelled.status, TurnRunStatus::Cancelled);
    let terminal = journaled.terminal_event.unwrap();
    assert_eq!(terminal.event, AgentEvent::TurnCancelled { usage });
    let recovered = store
        .finish_turn_cancellation_and_append_event(
            cancellation_turn.id,
            cancellation_token,
            acknowledged_at + Duration::hours(1),
            usage,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        recovered.outcome,
        FinishTurnCancellationOutcome::Existing(cancelled)
    );
    assert_eq!(recovered.terminal_event, Some(terminal));

    let event_chat = sample_chat();
    store.create_chat(&event_chat).await.unwrap();
    let barrier = Arc::new(tokio::sync::Barrier::new(16));
    let mut writers = Vec::new();
    for index in 0..16 {
        let store = store.clone();
        let barrier = barrier.clone();
        writers.push(tokio::spawn(async move {
            barrier.wait().await;
            store
                .append_event(
                    event_chat.id,
                    &AgentEvent::TextDelta {
                        text: format!("postgres delta {index}"),
                    },
                )
                .await
                .unwrap()
        }));
    }
    let mut assigned = Vec::new();
    for writer in writers {
        assigned.push(writer.await.unwrap());
    }
    assigned.sort_unstable();
    assert_eq!(assigned, (1..=16).collect::<Vec<_>>());
    let events = store.list_events(event_chat.id, 0).await.unwrap();
    assert_eq!(events.len(), 16);
    assert_eq!(
        events.iter().map(|event| event.seq).collect::<Vec<_>>(),
        (1..=16).collect::<Vec<_>>()
    );

    let turn_event_chat = sample_chat();
    store.create_chat(&turn_event_chat).await.unwrap();
    let turn_event = match store
        .accept_turn(TurnId::new(), turn_event_chat.id, "gpt-5", "event input")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    let event_claimed_at = turn_event.available_at + Duration::seconds(1);
    let event_lease_token = uuid::Uuid::new_v4();
    store
        .claim_turn_run(
            event_lease_token,
            event_claimed_at,
            event_claimed_at + Duration::minutes(1),
        )
        .await
        .unwrap()
        .turn
        .unwrap();
    let started = AgentEvent::TurnStarted {
        turn_id: turn_event.id,
    };
    assert_eq!(
        store
            .append_turn_event(
                turn_event_chat.id,
                turn_event.id,
                event_lease_token,
                1,
                event_claimed_at,
                &started,
            )
            .await
            .unwrap(),
        Some(1)
    );
    let turn_event_output = Message {
        id: MessageId::new(),
        chat_id: turn_event_chat.id,
        turn_id: turn_event.id,
        role: Role::Assistant,
        content: "event turn complete".into(),
        created_at: event_claimed_at + Duration::seconds(1),
    };
    store
        .complete_turn_run(
            turn_event.id,
            event_lease_token,
            turn_event_output.created_at,
            &turn_event_output,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        store
            .append_turn_event(
                turn_event_chat.id,
                turn_event.id,
                event_lease_token,
                1,
                event_claimed_at + Duration::hours(1),
                &started,
            )
            .await
            .unwrap(),
        Some(1),
        "postgres exact retries recover the original sequence after terminal state"
    );
    assert_eq!(
        store
            .append_turn_event(
                turn_event_chat.id,
                turn_event.id,
                event_lease_token,
                2,
                event_claimed_at + Duration::seconds(2),
                &AgentEvent::TextDelta {
                    text: "late".into(),
                },
            )
            .await
            .unwrap(),
        None,
        "postgres terminal state fences a new stale event"
    );
    assert!(store
        .append_event(turn_event_chat.id, &started)
        .await
        .is_err());
    assert!(store
        .append_turn_event(
            event_chat.id,
            turn_event.id,
            event_lease_token,
            2,
            event_claimed_at,
            &started,
        )
        .await
        .is_err());
    assert!(store
        .append_turn_event(
            turn_event_chat.id,
            turn_event.id,
            event_lease_token,
            2,
            event_claimed_at,
            &AgentEvent::TurnCompleted {
                usage: Usage::default(),
                stop_reason: StopReason::EndTurn,
            },
        )
        .await
        .is_err());
}
