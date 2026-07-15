#![cfg(feature = "postgres")]

use std::{sync::Arc, time::Duration as StdDuration};

use chrono::{Duration, Utc};
use openwave_core::{
    AcceptAgentRunOutcome, AcceptToolCallOutcome, AcceptTurnOutcome, AcceptTurnSteerOutcome,
    AgentEvent, AgentRunExecution, AgentRunId, AgentRunStatus, ApplyTurnSteerOutcome,
    BeginRootAttachmentChange, BeginRootAttachmentChangeOutcome, CallId, Chat, ChatId,
    ChatRootAttachment, ClaimClientToolCallOutcome, ClientToolCallRequest, CompleteTurnRunOutcome,
    DbStore, FinishRootAttachmentChangeOutcome, FinishTurnCancellationOutcome,
    HeartbeatClientToolCallOutcome, HostRootId, Message, MessageId, ParkTurnForClientCallOutcome,
    Project, ProjectId, RecordTurnFailureOutcome, RequestTurnCancellationOutcome,
    ResolveToolCallOutcome, Role, RootAttachmentChangeAction, RootAttachmentChangeId,
    RootAttachmentChangeTerminal, RootAttachmentOrigin, StopReason, Store, ToolCallExecution,
    ToolCallRecord, ToolCallResolution, ToolCallStatus, TurnCheckpointProgress, TurnFailureRetry,
    TurnId, TurnRunStatus, TurnSteerId, TurnSteerStatus, Usage,
};
use sea_orm::{ConnectionTrait, Database, DatabaseBackend, Statement, TransactionTrait};

static POSTGRES_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn utc_now_at_postgres_precision() -> chrono::DateTime<Utc> {
    chrono::DateTime::<Utc>::from_timestamp_micros(Utc::now().timestamp_micros()).unwrap()
}

fn sample_chat() -> Chat {
    Chat {
        id: ChatId::new(),
        project_id: None,
        title: None,
        model: None,
        attachment_revision: 0,
        root_attachments: Vec::new(),
        created_at: utc_now_at_postgres_precision(),
    }
}

#[tokio::test]
async fn postgres_agent_runs_enforce_foreground_parentage_and_idempotency() {
    let _guard = POSTGRES_TEST_LOCK.lock().await;
    let url = match std::env::var("OPENWAVE_POSTGRES_TEST_URL") {
        Ok(url) => url,
        Err(_) if std::env::var_os("OPENWAVE_REQUIRE_POSTGRES_TEST").is_some() => {
            panic!("OPENWAVE_POSTGRES_TEST_URL must name an isolated test database")
        }
        Err(_) => return,
    };
    let store = DbStore::connect(&url).await.unwrap();
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let foreground_id = AgentRunId::foreground_for_chat(chat.id);
    let foreground = store
        .get_agent_run(foreground_id)
        .await
        .unwrap()
        .expect("chat creation should create its foreground agent run");
    assert_eq!(foreground.depth, 0);
    assert_eq!(foreground.status, AgentRunStatus::Active);

    let child_id = AgentRunId::new();
    let spawn_call_id = CallId::new();
    let child = match store
        .accept_agent_run(
            child_id,
            chat.id,
            Some(foreground_id),
            Some(spawn_call_id),
            AgentRunExecution::Sandbox,
            Some("postgres child"),
        )
        .await
        .unwrap()
    {
        AcceptAgentRunOutcome::Accepted(run) => run,
        outcome => panic!("unexpected sandbox outcome: {outcome:?}"),
    };
    assert_eq!(child.depth, 1);
    assert_eq!(child.status, AgentRunStatus::Queued);
    assert!(matches!(
        store
            .accept_agent_run(
                child_id,
                chat.id,
                Some(foreground_id),
                Some(spawn_call_id),
                AgentRunExecution::Sandbox,
                Some("postgres child"),
            )
            .await
            .unwrap(),
        AcceptAgentRunOutcome::Existing(existing) if existing == child
    ));
    assert!(matches!(
        store
            .accept_agent_run(
                AgentRunId::new(),
                chat.id,
                Some(child_id),
                Some(CallId::new()),
                AgentRunExecution::Sandbox,
                Some("forbidden grandchild"),
            )
            .await
            .unwrap(),
        AcceptAgentRunOutcome::ParentUnavailable
    ));
}

#[tokio::test]
async fn postgres_agent_run_identity_collision_recovers_exactly_across_chats() {
    let _guard = POSTGRES_TEST_LOCK.lock().await;
    let url = match std::env::var("OPENWAVE_POSTGRES_TEST_URL") {
        Ok(url) => url,
        Err(_) if std::env::var_os("OPENWAVE_REQUIRE_POSTGRES_TEST").is_some() => {
            panic!("OPENWAVE_POSTGRES_TEST_URL must name an isolated test database")
        }
        Err(_) => return,
    };
    let store = DbStore::connect(&url).await.unwrap();
    let first_chat = sample_chat();
    let second_chat = sample_chat();
    store.create_chat(&first_chat).await.unwrap();
    store.create_chat(&second_chat).await.unwrap();
    let first_parent = AgentRunId::foreground_for_chat(first_chat.id);
    let second_parent = AgentRunId::foreground_for_chat(second_chat.id);

    let collision_id = AgentRunId::new();
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let mut tasks = Vec::new();
    for (chat_id, parent_id) in [
        (first_chat.id, first_parent),
        (second_chat.id, second_parent),
    ] {
        let store = store.clone();
        let barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            store
                .accept_agent_run(
                    collision_id,
                    chat_id,
                    Some(parent_id),
                    Some(CallId::new()),
                    AgentRunExecution::Sandbox,
                    Some("colliding child"),
                )
                .await
        }));
    }
    let mut accepted = 0;
    let mut conflicted = 0;
    for task in tasks {
        match task.await.unwrap() {
            Ok(AcceptAgentRunOutcome::Accepted(_)) => accepted += 1,
            Ok(AcceptAgentRunOutcome::IdentityConflict) => conflicted += 1,
            outcome => panic!("unexpected cross-chat collision outcome: {outcome:?}"),
        }
    }
    assert_eq!((accepted, conflicted), (1, 1));
}

#[tokio::test]
async fn postgres_concurrent_sandbox_claims_respect_global_and_per_chat_limits() {
    let _guard = POSTGRES_TEST_LOCK.lock().await;
    let url = match std::env::var("OPENWAVE_POSTGRES_TEST_URL") {
        Ok(url) => url,
        Err(_) if std::env::var_os("OPENWAVE_REQUIRE_POSTGRES_TEST").is_some() => {
            panic!("OPENWAVE_POSTGRES_TEST_URL must name an isolated test database")
        }
        Err(_) => return,
    };
    let store = Arc::new(DbStore::connect(&url).await.unwrap());
    let first_chat = sample_chat();
    let second_chat = sample_chat();
    store.create_chat(&first_chat).await.unwrap();
    store.create_chat(&second_chat).await.unwrap();
    for chat_id in [first_chat.id, first_chat.id, second_chat.id, second_chat.id] {
        let _accepted = match store
            .accept_agent_run(
                AgentRunId::new(),
                chat_id,
                Some(AgentRunId::foreground_for_chat(chat_id)),
                Some(CallId::new()),
                AgentRunExecution::Sandbox,
                Some("postgres concurrent claim"),
            )
            .await
            .unwrap()
        {
            AcceptAgentRunOutcome::Accepted(run) => run,
            outcome => panic!("unexpected sandbox outcome: {outcome:?}"),
        };
    }
    let barrier = Arc::new(tokio::sync::Barrier::new(8));
    let mut claimers = Vec::new();
    for _ in 0..8 {
        claimers.push(DbStore::connect(&url).await.unwrap());
    }
    let mut tasks = Vec::new();
    for store in claimers {
        let barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            store
                .claim_agent_run(uuid::Uuid::new_v4(), Duration::minutes(1), 2, 1)
                .await
                .unwrap()
        }));
    }
    let claimed = futures::future::join_all(tasks)
        .await
        .into_iter()
        .filter_map(|result| result.unwrap())
        .collect::<Vec<_>>();
    assert_eq!(claimed.len(), 2);
    assert_ne!(claimed[0].chat_id, claimed[1].chat_id);
}

#[tokio::test]
async fn postgres_sandbox_claim_uses_statement_time_after_scheduler_lock_wait() {
    let _guard = POSTGRES_TEST_LOCK.lock().await;
    let url = match std::env::var("OPENWAVE_POSTGRES_TEST_URL") {
        Ok(url) => url,
        Err(_) if std::env::var_os("OPENWAVE_REQUIRE_POSTGRES_TEST").is_some() => {
            panic!("OPENWAVE_POSTGRES_TEST_URL must name an isolated test database")
        }
        Err(_) => return,
    };
    let store = DbStore::connect(&url).await.unwrap();
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let child_id = AgentRunId::new();
    store
        .accept_agent_run(
            child_id,
            chat.id,
            Some(AgentRunId::foreground_for_chat(chat.id)),
            Some(CallId::new()),
            AgentRunExecution::Sandbox,
            Some("scheduler lock clock regression"),
        )
        .await
        .unwrap();

    let blocker = Database::connect(&url).await.unwrap();
    blocker
        .execute(Statement::from_string(
            DatabaseBackend::Postgres,
            format!(
                "UPDATE agent_run \
                 SET created_at = TIMESTAMPTZ '2000-01-01 00:00:00+00', \
                     available_at = TIMESTAMPTZ '2000-01-01 00:00:00+00', \
                     updated_at = clock_timestamp() \
                 WHERE id = '{}'",
                child_id.0
            ),
        ))
        .await
        .unwrap();
    let transaction = blocker.begin().await.unwrap();
    transaction
        .execute(Statement::from_string(
            DatabaseBackend::Postgres,
            "UPDATE agent_run_claim_lock SET id = id WHERE id = 1",
        ))
        .await
        .unwrap();

    let claimant = DbStore::connect(&url).await.unwrap();
    let claim = tokio::spawn(async move {
        claimant
            .claim_agent_run(
                uuid::Uuid::new_v4(),
                Duration::milliseconds(100),
                1_024,
                1_024,
            )
            .await
            .unwrap()
            .unwrap()
    });
    // Give the claimant time to begin its transaction and block on the row.
    // The wait is deliberately longer than the requested lease duration.
    tokio::time::sleep(StdDuration::from_millis(500)).await;
    transaction.commit().await.unwrap();

    let claimed = claim.await.unwrap();
    assert_eq!(claimed.id, child_id);
    assert!(claimed.lease_expires_at.unwrap() > Utc::now());
    // The integration suite shares an isolated database, so leave no live
    // scheduler capacity behind for its independent concurrency cases.
    blocker
        .execute(Statement::from_string(
            DatabaseBackend::Postgres,
            format!(
                "UPDATE agent_run \
                 SET status = 'completed', lease_token = NULL, lease_expires_at = NULL, \
                     finished_at = clock_timestamp(), updated_at = clock_timestamp() \
                 WHERE id = '{}'",
                child_id.0
            ),
        ))
        .await
        .unwrap();
}

#[tokio::test]
async fn postgres_ordered_root_projection_roundtrips_and_snapshots_atomically() {
    let _guard = POSTGRES_TEST_LOCK.lock().await;
    let url = match std::env::var("OPENWAVE_POSTGRES_TEST_URL") {
        Ok(url) => url,
        Err(_) if std::env::var_os("OPENWAVE_REQUIRE_POSTGRES_TEST").is_some() => {
            panic!("OPENWAVE_POSTGRES_TEST_URL must name an isolated test database")
        }
        Err(_) => return,
    };
    let store = DbStore::connect(&url).await.unwrap();
    let root_b = HostRootId::from_uuid(uuid::Uuid::new_v4()).unwrap();
    let root_a = HostRootId::from_uuid(uuid::Uuid::new_v4()).unwrap();
    let project = Project {
        id: ProjectId::new(),
        title: Some("postgres roots".into()),
        attachment_revision: 1,
        root_attachments: vec![root_b, root_a],
        created_at: utc_now_at_postgres_precision(),
    };
    store.create_project(&project).await.unwrap();
    assert_eq!(
        store.get_project(project.id).await.unwrap(),
        Some(project.clone())
    );

    let mut base = sample_chat();
    base.project_id = Some(project.id);
    let chat = store
        .create_chat_with_project_defaults(&base)
        .await
        .unwrap();
    assert_eq!(chat.attachment_revision, 1);
    assert_eq!(
        chat.root_attachments
            .iter()
            .map(|attachment| (attachment.root_id, attachment.origin))
            .collect::<Vec<_>>(),
        vec![
            (root_b, RootAttachmentOrigin::ProjectDefault),
            (root_a, RootAttachmentOrigin::ProjectDefault),
        ]
    );
    assert_eq!(store.get_chat(chat.id).await.unwrap(), Some(chat));
}

#[tokio::test]
async fn postgres_client_execution_claim_has_one_winner() {
    let _guard = POSTGRES_TEST_LOCK.lock().await;
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
    let created_at = chrono::DateTime::<Utc>::from_timestamp(1_800_000_000, 123_456_789).unwrap();
    let call = ToolCallRecord {
        id: CallId::new(),
        chat_id: chat.id,
        turn_id: TurnId::new(),
        provider_id: "postgres_client_call".into(),
        name: "select_folder".into(),
        arguments: serde_json::json!({"reason": "test"}),
        execution: ToolCallExecution::Client,
        status: ToolCallStatus::Pending,
        result: None,
        error_code: None,
        error_detail: None,
        client_executor_id: None,
        client_lease_expires_at: None,
        created_at,
        resolved_at: None,
    };
    assert!(matches!(
        store.accept_tool_call(&call).await.unwrap(),
        AcceptToolCallOutcome::Accepted(_)
    ));
    assert!(matches!(
        store.accept_tool_call(&call).await.unwrap(),
        AcceptToolCallOutcome::Existing(_)
    ));

    let claim_at = created_at + Duration::seconds(1);
    let lease_expires_at = claim_at + Duration::minutes(1);
    let barrier = Arc::new(tokio::sync::Barrier::new(8));
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let store = store.clone();
        let barrier = barrier.clone();
        let executor = uuid::Uuid::new_v4();
        let lease_token = uuid::Uuid::new_v4();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            (
                executor,
                lease_token,
                store
                    .claim_client_tool_call(
                        call.id,
                        chat.id,
                        executor,
                        lease_token,
                        claim_at,
                        lease_expires_at,
                    )
                    .await
                    .unwrap(),
            )
        }));
    }
    let mut winner = None;
    for task in tasks {
        let (executor, requested_lease_token, outcome) = task.await.unwrap();
        match outcome {
            ClaimClientToolCallOutcome::Claimed(claim) => {
                assert_eq!(claim.lease_token, requested_lease_token);
                assert!(
                    winner.replace((executor, claim.lease_token)).is_none(),
                    "multiple claim winners"
                );
            }
            ClaimClientToolCallOutcome::Unavailable => {}
            outcome => panic!("unexpected concurrent claim outcome: {outcome:?}"),
        }
    }
    let (winner, lease_token) = winner.expect("one client executor claimed the call");
    assert!(matches!(
        store
            .claim_client_tool_call(
                call.id,
                chat.id,
                winner,
                lease_token,
                claim_at + Duration::microseconds(1),
                lease_expires_at + Duration::microseconds(1),
            )
            .await
            .unwrap(),
        ClaimClientToolCallOutcome::Existing(_)
    ));
    let extended_expiry = lease_expires_at + Duration::minutes(1);
    assert_eq!(
        store
            .heartbeat_client_tool_call(
                call.id,
                chat.id,
                lease_token,
                claim_at + Duration::seconds(1),
                extended_expiry,
            )
            .await
            .unwrap(),
        HeartbeatClientToolCallOutcome::Extended
    );
    assert_eq!(
        store
            .heartbeat_client_tool_call(
                call.id,
                chat.id,
                lease_token,
                claim_at + Duration::seconds(1),
                extended_expiry,
            )
            .await
            .unwrap(),
        HeartbeatClientToolCallOutcome::Existing
    );
    let resolved_at = claim_at + Duration::seconds(1);
    let resolution = ToolCallResolution::Completed {
        result: "selected".into(),
    };
    assert_eq!(
        store
            .resolve_client_tool_call(
                call.id,
                chat.id,
                lease_token,
                resolved_at + Duration::microseconds(1),
                &resolution,
                resolved_at + Duration::microseconds(1),
            )
            .await
            .unwrap(),
        ResolveToolCallOutcome::Resolved
    );
    assert_eq!(
        store
            .resolve_client_tool_call(
                call.id,
                chat.id,
                lease_token,
                resolved_at,
                &resolution,
                resolved_at,
            )
            .await
            .unwrap(),
        ResolveToolCallOutcome::Existing
    );
}

#[tokio::test]
async fn postgres_turn_acceptance_claims_and_receipts_are_atomic() {
    let _guard = POSTGRES_TEST_LOCK.lock().await;
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
                    0,
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
            outcome => panic!("unexpected completion outcome: {outcome:?}"),
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
                    0,
                    Usage::default(),
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
            0,
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

    let steer_chat = sample_chat();
    store.create_chat(&steer_chat).await.unwrap();
    let steer_turn = match store
        .accept_turn(TurnId::new(), steer_chat.id, "gpt-5", "steer input")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected steer turn acceptance: {outcome:?}"),
    };
    let steer_id = TurnSteerId::new();
    let barrier = Arc::new(tokio::sync::Barrier::new(4));
    let mut steer_tasks = Vec::new();
    for _ in 0..4 {
        let store = store.clone();
        let barrier = barrier.clone();
        steer_tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            store
                .accept_turn_steer(
                    steer_id,
                    steer_turn.id,
                    steer_chat.id,
                    "postgres steer",
                    true,
                )
                .await
                .unwrap()
        }));
    }
    let mut steer_accepted = 0;
    let mut steer_existing = 0;
    for task in steer_tasks {
        match task.await.unwrap() {
            AcceptTurnSteerOutcome::Accepted(_) => steer_accepted += 1,
            AcceptTurnSteerOutcome::Existing(_) => steer_existing += 1,
            outcome => panic!("unexpected concurrent steer acceptance: {outcome:?}"),
        }
    }
    assert_eq!((steer_accepted, steer_existing), (1, 3));

    let steer_lease = uuid::Uuid::new_v4();
    let steer_claimed_at = Utc::now();
    store
        .claim_turn_run(
            steer_lease,
            steer_claimed_at,
            steer_claimed_at + Duration::minutes(1),
        )
        .await
        .unwrap()
        .turn
        .unwrap();
    assert_eq!(
        store
            .list_pending_turn_steers(steer_turn.id, steer_lease, Utc::now())
            .await
            .unwrap()
            .unwrap()
            .iter()
            .map(|steer| steer.id)
            .collect::<Vec<_>>(),
        vec![steer_id]
    );
    let preceding = Message {
        id: MessageId::new(),
        chat_id: steer_chat.id,
        turn_id: steer_turn.id,
        role: Role::Assistant,
        content: "postgres candidate".into(),
        created_at: Utc::now(),
    };
    let applied = store
        .apply_turn_steer(
            steer_turn.id,
            steer_lease,
            steer_id,
            1,
            Some(&preceding),
            Utc::now(),
        )
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(applied.outcome, ApplyTurnSteerOutcome::Applied(_)));
    assert_eq!(
        applied.event.event,
        AgentEvent::UserSteered {
            content: "postgres steer".into(),
        }
    );
    assert_eq!(
        store
            .list_messages(steer_chat.id)
            .await
            .unwrap()
            .iter()
            .map(|message| (message.role, message.content.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (Role::User, "steer input"),
            (Role::Assistant, "postgres candidate"),
            (Role::User, "postgres steer"),
        ]
    );

    let second_steer_id = TurnSteerId::new();
    assert!(matches!(
        store
            .accept_turn_steer(
                second_steer_id,
                steer_turn.id,
                steer_chat.id,
                "apply before completion",
                false,
            )
            .await
            .unwrap(),
        AcceptTurnSteerOutcome::Accepted(_)
    ));
    let steer_completed_at = Utc::now();
    assert!(matches!(
        store
            .complete_turn_run(
                steer_turn.id,
                steer_lease,
                0,
                steer_completed_at,
                &Message {
                    id: MessageId::new(),
                    chat_id: steer_chat.id,
                    turn_id: steer_turn.id,
                    role: Role::Assistant,
                    content: "stale steer completion".into(),
                    created_at: steer_completed_at,
                },
            )
            .await
            .unwrap(),
        Some(CompleteTurnRunOutcome::SteerPending(_))
    ));
    let second_steer = store
        .apply_turn_steer(
            steer_turn.id,
            steer_lease,
            second_steer_id,
            2,
            None,
            Utc::now(),
        )
        .await
        .unwrap()
        .unwrap();
    let second_steer = match second_steer.outcome {
        ApplyTurnSteerOutcome::Applied(steer) => steer,
        outcome => panic!("unexpected postgres second steer: {outcome:?}"),
    };
    let fresh_completed_at = second_steer.resolved_at.unwrap() + chrono::Duration::microseconds(1);
    store
        .complete_turn_run(
            steer_turn.id,
            steer_lease,
            1,
            fresh_completed_at,
            &Message {
                id: MessageId::new(),
                chat_id: steer_chat.id,
                turn_id: steer_turn.id,
                role: Role::Assistant,
                content: "fresh steer completion".into(),
                created_at: fresh_completed_at,
            },
        )
        .await
        .unwrap()
        .unwrap();
    let recovered = store
        .apply_turn_steer(
            steer_turn.id,
            steer_lease,
            steer_id,
            1,
            Some(&preceding),
            Utc::now(),
        )
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        recovered.outcome,
        ApplyTurnSteerOutcome::Existing(_)
    ));
    assert_eq!(recovered.event, applied.event);
    assert!(matches!(
        store
            .accept_turn_steer(
                second_steer_id,
                steer_turn.id,
                steer_chat.id,
                "apply before completion",
                false,
            )
            .await
            .unwrap(),
        AcceptTurnSteerOutcome::Existing(steer)
            if steer.status == TurnSteerStatus::Applied
    ));

    let registry_steer_chat = sample_chat();
    let registry_message_chat = sample_chat();
    store.create_chat(&registry_steer_chat).await.unwrap();
    store.create_chat(&registry_message_chat).await.unwrap();
    let registry_steer_turn = match store
        .accept_turn(
            TurnId::new(),
            registry_steer_chat.id,
            "gpt-5",
            "registry steer",
        )
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected registry steer turn: {outcome:?}"),
    };
    let registry_message_turn = match store
        .accept_turn(
            TurnId::new(),
            registry_message_chat.id,
            "gpt-5",
            "registry message",
        )
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected registry message turn: {outcome:?}"),
    };
    let shared_identity = TurnSteerId::new();
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let steer_store = store.clone();
    let steer_barrier = barrier.clone();
    let steer_task = tokio::spawn(async move {
        steer_barrier.wait().await;
        steer_store
            .accept_turn_steer(
                shared_identity,
                registry_steer_turn.id,
                registry_steer_chat.id,
                "shared postgres identity",
                false,
            )
            .await
            .unwrap()
    });
    let message_store = store.clone();
    let message_barrier = barrier.clone();
    let message_task = tokio::spawn(async move {
        message_barrier.wait().await;
        message_store
            .append_message(&Message {
                id: MessageId(shared_identity.0),
                chat_id: registry_message_chat.id,
                turn_id: registry_message_turn.id,
                role: Role::Assistant,
                content: "shared postgres identity".into(),
                created_at: Utc::now(),
            })
            .await
    });
    let steer_result = steer_task.await.unwrap();
    let message_result = message_task.await.unwrap();
    match steer_result {
        AcceptTurnSteerOutcome::Accepted(_) => assert!(message_result.is_err()),
        AcceptTurnSteerOutcome::IdentityConflict => {
            message_result.expect("postgres message won shared identity")
        }
        outcome => panic!("unexpected postgres registry race: {outcome:?}"),
    }
    store
        .request_turn_cancellation(registry_steer_turn.id, Utc::now())
        .await
        .unwrap();
    store
        .request_turn_cancellation(registry_message_turn.id, Utc::now())
        .await
        .unwrap();

    let client_wait_chat = sample_chat();
    store.create_chat(&client_wait_chat).await.unwrap();
    let client_wait_turn = match store
        .accept_turn(
            TurnId::new(),
            client_wait_chat.id,
            "gpt-5",
            "postgres native action",
        )
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected client-wait turn acceptance: {outcome:?}"),
    };
    let client_wait_claimed_at = client_wait_turn.available_at + Duration::seconds(1);
    let client_wait_turn_token = uuid::Uuid::new_v4();
    let client_wait_claim = store
        .claim_turn_run(
            client_wait_turn_token,
            client_wait_claimed_at,
            client_wait_claimed_at + Duration::minutes(1),
        )
        .await
        .unwrap()
        .turn
        .unwrap();
    assert_eq!(client_wait_claim.id, client_wait_turn.id);
    let client_request = ClientToolCallRequest {
        id: CallId::new(),
        chat_id: client_wait_chat.id,
        turn_id: client_wait_turn.id,
        provider_id: "native".into(),
        name: "connect_folder".into(),
        arguments: serde_json::json!({"reason": "postgres state test"}),
    };
    let client_wait_parked_at = client_wait_claimed_at + Duration::seconds(1);
    let client_wait_progress = TurnCheckpointProgress {
        model_steps: 2,
        usage: Usage {
            input_tokens: 13,
            output_tokens: 8,
            cache_read_input_tokens: 5,
            cache_creation_input_tokens: 3,
        },
    };
    assert!(matches!(
        store
            .park_turn_for_client_tool_call(
                client_wait_turn.id,
                client_wait_turn_token,
                0,
                client_wait_progress,
                client_wait_parked_at,
                &client_request,
            )
            .await
            .unwrap()
            .unwrap(),
        ParkTurnForClientCallOutcome::Parked { turn, wait, .. }
            if turn.status == TurnRunStatus::WaitingForClient
                && turn.model_steps == client_wait_progress.model_steps
                && turn.usage == client_wait_progress.usage
                && wait.progress == client_wait_progress
    ));
    let client_token = uuid::Uuid::new_v4();
    let client_claimed_at = client_wait_parked_at + Duration::seconds(1);
    assert!(matches!(
        store
            .claim_client_tool_call(
                client_request.id,
                client_wait_chat.id,
                uuid::Uuid::new_v4(),
                client_token,
                client_claimed_at,
                client_claimed_at + Duration::minutes(1),
            )
            .await
            .unwrap(),
        ClaimClientToolCallOutcome::Claimed(_)
    ));
    let client_resolved_at = client_claimed_at + Duration::seconds(1);
    assert_eq!(
        store
            .resolve_client_tool_call(
                client_request.id,
                client_wait_chat.id,
                client_token,
                client_resolved_at,
                &ToolCallResolution::Completed {
                    result: "root-postgres".into(),
                },
                client_resolved_at,
            )
            .await
            .unwrap(),
        ResolveToolCallOutcome::Resolved
    );
    assert_eq!(
        store
            .get_turn_run(client_wait_turn.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        TurnRunStatus::Resuming
    );
    let resumed_token = uuid::Uuid::new_v4();
    let resumed = store
        .claim_turn_run(
            resumed_token,
            client_resolved_at,
            client_resolved_at + Duration::minutes(1),
        )
        .await
        .unwrap()
        .turn
        .unwrap();
    assert_eq!(resumed.id, client_wait_turn.id);
    assert_eq!((resumed.attempt_count, resumed.claim_count), (1, 2));
    store
        .request_turn_cancellation(resumed.id, client_resolved_at + Duration::seconds(1))
        .await
        .unwrap();
    store
        .finish_turn_cancellation(
            resumed.id,
            resumed_token,
            client_resolved_at + Duration::seconds(2),
        )
        .await
        .unwrap();

    // Keep this global-queue fixture last: its winning turn deliberately remains
    // queued so the race proves only acceptance rollback/recovery behavior.
    let first_collision_chat = sample_chat();
    let second_collision_chat = sample_chat();
    store.create_chat(&first_collision_chat).await.unwrap();
    store.create_chat(&second_collision_chat).await.unwrap();
    let collision_turn_id = TurnId::new();
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let mut collision_tasks = Vec::new();
    for collision_chat_id in [first_collision_chat.id, second_collision_chat.id] {
        let store = store.clone();
        let barrier = barrier.clone();
        collision_tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            store
                .accept_turn(
                    collision_turn_id,
                    collision_chat_id,
                    "gpt-5",
                    "colliding input",
                )
                .await
        }));
    }
    let mut collision_accepted = 0;
    let mut collision_conflicted = 0;
    for task in collision_tasks {
        match task.await.unwrap() {
            Ok(AcceptTurnOutcome::Accepted(_)) => collision_accepted += 1,
            Ok(AcceptTurnOutcome::IdentityConflict) => collision_conflicted += 1,
            outcome => panic!("unexpected cross-chat collision outcome: {outcome:?}"),
        }
    }
    assert_eq!((collision_accepted, collision_conflicted), (1, 1));
    assert_eq!(
        store
            .list_messages(first_collision_chat.id)
            .await
            .unwrap()
            .len()
            + store
                .list_messages(second_collision_chat.id)
                .await
                .unwrap()
                .len(),
        1
    );
}

#[tokio::test]
async fn postgres_root_attachment_begin_and_finish_have_one_concurrent_winner() {
    let _guard = POSTGRES_TEST_LOCK.lock().await;
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
    let executor_id = uuid::Uuid::new_v4();
    let created_at = chrono::DateTime::<Utc>::from_timestamp(1_810_000_000, 123_456_789).unwrap();
    let canonical_created_at =
        chrono::DateTime::<Utc>::from_timestamp_micros(created_at.timestamp_micros()).unwrap();
    let requests = [
        BeginRootAttachmentChange {
            id: RootAttachmentChangeId::new(),
            chat_id: chat.id,
            executor_id,
            root_id: HostRootId::from_uuid(uuid::Uuid::new_v4()).unwrap(),
            action: RootAttachmentChangeAction::Attach,
            expected_attachment_revision: 0,
            created_at,
        },
        BeginRootAttachmentChange {
            id: RootAttachmentChangeId::new(),
            chat_id: chat.id,
            executor_id,
            root_id: HostRootId::from_uuid(uuid::Uuid::new_v4()).unwrap(),
            action: RootAttachmentChangeAction::Attach,
            expected_attachment_revision: 0,
            created_at,
        },
    ];
    let barrier = Arc::new(tokio::sync::Barrier::new(requests.len()));
    let mut tasks = Vec::new();
    for request in requests {
        let store = store.clone();
        let barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            store.begin_root_attachment_change(&request).await.unwrap()
        }));
    }

    let mut begun = None;
    let mut busy = false;
    for task in tasks {
        match task.await.unwrap() {
            BeginRootAttachmentChangeOutcome::Begun(change) => {
                assert!(begun.replace(change).is_none(), "multiple begin winners");
            }
            BeginRootAttachmentChangeOutcome::ChatBusy => {
                assert!(!busy, "multiple busy outcomes");
                busy = true;
            }
            outcome => panic!("unexpected concurrent begin outcome: {outcome:?}"),
        }
    }
    let begun = begun.expect("one attachment change began");
    assert!(busy, "one concurrent begin was busy");
    assert_eq!(begun.created_at, canonical_created_at);
    let winning_request = requests
        .into_iter()
        .find(|request| request.id == begun.id)
        .unwrap();
    assert_eq!(
        store
            .begin_root_attachment_change(&winning_request)
            .await
            .unwrap(),
        BeginRootAttachmentChangeOutcome::Existing(begun.clone())
    );

    let terminal = RootAttachmentChangeTerminal::Completed {
        broker_changed: true,
        broker_currently_attached: true,
    };
    let finished_at = [
        chrono::DateTime::<Utc>::from_timestamp(1_810_000_001, 234_567_891).unwrap(),
        chrono::DateTime::<Utc>::from_timestamp(1_810_000_002, 345_678_912).unwrap(),
    ];
    let canonical_finished_at = finished_at.map(|value| {
        chrono::DateTime::<Utc>::from_timestamp_micros(value.timestamp_micros()).unwrap()
    });
    let barrier = Arc::new(tokio::sync::Barrier::new(finished_at.len()));
    let mut tasks = Vec::new();
    for value in finished_at {
        let store = store.clone();
        let barrier = barrier.clone();
        let terminal = terminal.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            store
                .finish_root_attachment_change(begun.id, executor_id, &terminal, value)
                .await
                .unwrap()
        }));
    }

    let mut finished = None;
    let mut existing = None;
    for task in tasks {
        match task.await.unwrap() {
            FinishRootAttachmentChangeOutcome::Finished(change) => {
                assert!(
                    finished.replace(change).is_none(),
                    "multiple finish winners"
                );
            }
            FinishRootAttachmentChangeOutcome::Existing(change) => {
                assert!(
                    existing.replace(change).is_none(),
                    "multiple recovered finishes"
                );
            }
            outcome => panic!("unexpected concurrent finish outcome: {outcome:?}"),
        }
    }
    let finished = finished.expect("one attachment change finished");
    assert_eq!(existing, Some(finished.clone()));
    assert!(canonical_finished_at.contains(&finished.finished_at.unwrap()));
    assert_eq!(
        store
            .finish_root_attachment_change(
                begun.id,
                executor_id,
                &terminal,
                finished_at[0] + Duration::minutes(1),
            )
            .await
            .unwrap(),
        FinishRootAttachmentChangeOutcome::Existing(finished.clone())
    );
    let projected = store.get_chat(chat.id).await.unwrap().unwrap();
    assert_eq!(projected.attachment_revision, 1);
    assert_eq!(
        projected.root_attachments,
        vec![ChatRootAttachment {
            root_id: begun.root_id,
            origin: RootAttachmentOrigin::Conversation,
        }]
    );
}

#[tokio::test]
async fn postgres_root_attachment_operation_id_collision_has_no_loser_side_effect() {
    let _guard = POSTGRES_TEST_LOCK.lock().await;
    let url = match std::env::var("OPENWAVE_POSTGRES_TEST_URL") {
        Ok(url) => url,
        Err(_) if std::env::var_os("OPENWAVE_REQUIRE_POSTGRES_TEST").is_some() => {
            panic!("OPENWAVE_POSTGRES_TEST_URL must name an isolated test database")
        }
        Err(_) => return,
    };
    let store = Arc::new(DbStore::connect(&url).await.unwrap());
    let chats = [sample_chat(), sample_chat()];
    for chat in &chats {
        store.create_chat(chat).await.unwrap();
    }
    let operation_id = RootAttachmentChangeId::new();
    let executor_id = uuid::Uuid::new_v4();
    let requests = chats.map(|chat| BeginRootAttachmentChange {
        id: operation_id,
        chat_id: chat.id,
        executor_id,
        root_id: HostRootId::from_uuid(uuid::Uuid::new_v4()).unwrap(),
        action: RootAttachmentChangeAction::Attach,
        expected_attachment_revision: 0,
        created_at: chrono::DateTime::<Utc>::from_timestamp(1_810_000_005, 345_678_912).unwrap(),
    });
    let barrier = Arc::new(tokio::sync::Barrier::new(requests.len()));
    let mut tasks = Vec::new();
    for request in requests {
        let store = store.clone();
        let barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            let outcome = store.begin_root_attachment_change(&request).await.unwrap();
            (request, outcome)
        }));
    }

    let mut winner = None;
    let mut loser = None;
    for task in tasks {
        let (request, outcome) = task.await.unwrap();
        match outcome {
            BeginRootAttachmentChangeOutcome::Begun(change) => {
                assert_eq!(change.chat_id, request.chat_id);
                assert_eq!(change.root_id, request.root_id);
                assert!(winner.replace(request).is_none(), "multiple begin winners");
            }
            BeginRootAttachmentChangeOutcome::IdentityConflict => {
                assert!(
                    loser.replace(request).is_none(),
                    "multiple identity conflicts"
                );
            }
            outcome => panic!("unexpected operation ID collision outcome: {outcome:?}"),
        }
    }
    let winner = winner.expect("one operation ID collision began");
    let loser = loser.expect("one operation ID collision conflicted");

    let winning_chat = store.get_chat(winner.chat_id).await.unwrap().unwrap();
    assert_eq!(winning_chat.attachment_revision, 1);
    assert_eq!(
        winning_chat.root_attachments,
        vec![ChatRootAttachment {
            root_id: winner.root_id,
            origin: RootAttachmentOrigin::Conversation,
        }]
    );
    let losing_chat = store.get_chat(loser.chat_id).await.unwrap().unwrap();
    assert_eq!(losing_chat.attachment_revision, 0);
    assert!(losing_chat.root_attachments.is_empty());
}

#[tokio::test]
async fn postgres_root_attachment_detach_compacts_the_ordered_projection() {
    let _guard = POSTGRES_TEST_LOCK.lock().await;
    let url = match std::env::var("OPENWAVE_POSTGRES_TEST_URL") {
        Ok(url) => url,
        Err(_) if std::env::var_os("OPENWAVE_REQUIRE_POSTGRES_TEST").is_some() => {
            panic!("OPENWAVE_POSTGRES_TEST_URL must name an isolated test database")
        }
        Err(_) => return,
    };
    let store = DbStore::connect(&url).await.unwrap();
    let roots = [
        HostRootId::from_uuid(uuid::Uuid::new_v4()).unwrap(),
        HostRootId::from_uuid(uuid::Uuid::new_v4()).unwrap(),
        HostRootId::from_uuid(uuid::Uuid::new_v4()).unwrap(),
    ];
    let mut chat = sample_chat();
    chat.attachment_revision = 7;
    chat.root_attachments = roots
        .into_iter()
        .map(|root_id| ChatRootAttachment {
            root_id,
            origin: RootAttachmentOrigin::Conversation,
        })
        .collect();
    store.create_chat(&chat).await.unwrap();
    let executor_id = uuid::Uuid::new_v4();
    let request = BeginRootAttachmentChange {
        id: RootAttachmentChangeId::new(),
        chat_id: chat.id,
        executor_id,
        root_id: roots[1],
        action: RootAttachmentChangeAction::Detach,
        expected_attachment_revision: 7,
        created_at: chrono::DateTime::<Utc>::from_timestamp(1_810_000_010, 456_789_123).unwrap(),
    };
    let pending = match store.begin_root_attachment_change(&request).await.unwrap() {
        BeginRootAttachmentChangeOutcome::Begun(change) => change,
        outcome => panic!("unexpected detach begin outcome: {outcome:?}"),
    };
    assert_eq!(pending.projection_position, Some(1));
    assert_eq!(pending.intent_revision, 7);

    let terminal = RootAttachmentChangeTerminal::Completed {
        broker_changed: true,
        broker_currently_attached: false,
    };
    let finished = match store
        .finish_root_attachment_change(
            request.id,
            executor_id,
            &terminal,
            chrono::DateTime::<Utc>::from_timestamp(1_810_000_011, 567_891_234).unwrap(),
        )
        .await
        .unwrap()
    {
        FinishRootAttachmentChangeOutcome::Finished(change) => change,
        outcome => panic!("unexpected detach finish outcome: {outcome:?}"),
    };
    assert_eq!(finished.result_revision, Some(8));
    assert_eq!(finished.projection_changed, Some(true));
    let projected = store.get_chat(chat.id).await.unwrap().unwrap();
    assert_eq!(projected.attachment_revision, 8);
    assert_eq!(
        projected
            .root_attachments
            .into_iter()
            .map(|attachment| attachment.root_id)
            .collect::<Vec<_>>(),
        vec![roots[0], roots[2]]
    );
}
