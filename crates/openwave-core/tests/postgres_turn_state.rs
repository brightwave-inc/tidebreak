#![cfg(feature = "postgres")]

use std::path::PathBuf;
use std::sync::Arc;

use chrono::{Duration, Utc};
use openwave_core::{
    AcceptTurnOutcome, Chat, ChatId, CompleteTurnRunOutcome, DbStore, Message, MessageId,
    RecordTurnFailureOutcome, Role, Store, TurnFailureRetry, TurnId, TurnRunStatus,
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
        match outcome.unwrap() {
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
            .unwrap(),
        Some(claimed)
    );

    assert_eq!(
        store
            .claim_turn_run(
                uuid::Uuid::new_v4(),
                lease_expires_at,
                lease_expires_at + Duration::minutes(1),
            )
            .await
            .unwrap(),
        None
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
            .unwrap(),
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
                .complete_turn_run(next.id, completion_token, output.created_at, &output)
                .await
                .unwrap()
                .unwrap()
        }));
    }
    let mut committed = 0;
    let mut existing = 0;
    for completion in completions {
        match completion.await.unwrap() {
            CompleteTurnRunOutcome::Completed(_) => committed += 1,
            CompleteTurnRunOutcome::Existing(_) => existing += 1,
        }
    }
    assert_eq!((committed, existing), (1, 1));
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
        .unwrap();
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let mut failures = Vec::new();
    for _ in 0..2 {
        let store = store.clone();
        let barrier = barrier.clone();
        failures.push(tokio::spawn(async move {
            barrier.wait().await;
            store
                .record_turn_run_failure(
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
    for failure in failures {
        match failure.await.unwrap() {
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
    }
    assert_eq!((recorded, failure_existing), (1, 1));
    assert_eq!(
        store
            .get_turn_run(failure_turn.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        TurnRunStatus::Failed
    );
}
