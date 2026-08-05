#![cfg(feature = "postgres")]

use std::{sync::Arc, time::Duration as StdDuration};

use chrono::{Duration, Utc};
use openwave_core::{
    AcceptToolCallOutcome, AcceptTurnOutcome, AcceptTurnSteerOutcome, AgentError, AgentEvent,
    AgentRun, AgentRunCancellationReason, AgentRunId, AgentRunInboxStatus, AgentRunResultPayload,
    AgentRunStatus, AgentRunTier, AgentRunWaitCondition, AgentRunWaitSetCheckpointRequest,
    AnswerUserQuestions, AnswerUserQuestionsOutcome, AnswerUserQuestionsRequest,
    ApplyTurnSteerOutcome, AssistantCitationInput, BeginRootAttachmentChange,
    BeginRootAttachmentChangeOutcome, CallId, Chat, ChatId, ChatRootAttachment,
    CheckpointSandboxSpawnOutcome, CitationLocator, ClaimClientToolCallOutcome,
    ClientToolCallRequest, CompleteTurnRunOutcome, DbStore, DeleteChatOutcome,
    DeleteProjectOutcome, DocumentId, DocumentSourceBlob, DocumentSourceUpsert, DocumentUpsert,
    FinishAgentRunCancellationOutcome, FinishRootAttachmentChangeOutcome,
    FinishTurnCancellationOutcome, HeartbeatClientToolCallOutcome, HostRootId, Message, MessageId,
    ParkTurnForAgentRunWaitSetOutcome, ParkTurnForClientCallOutcome, Project, ProjectId,
    RecordTurnFailureOutcome, RequestAgentRunCancellationOutcome, RequestTurnCancellationOutcome,
    ResolveToolCallOutcome, ResumeTurnForAgentRunWaitSetOutcome, Role, RootAttachmentChangeAction,
    RootAttachmentChangeId, RootAttachmentChangeTerminal, RootAttachmentOrigin,
    SandboxSpawnCheckpointRequest, SpawnSandboxAgentResult, StopReason, Store,
    SubmitAgentRunResultOutcome, ToolCallExecution, ToolCallRecord, ToolCallResolution,
    ToolCallStatus, TurnCheckpointProgress, TurnFailureRetry, TurnId, TurnRun, TurnRunStatus,
    TurnSteerId, TurnSteerStatus, Usage, UserQuestionAnswer, ASK_USER_QUESTIONS_TOOL,
};
use sea_orm::{ConnectionTrait, Database, DatabaseBackend, Statement, TransactionTrait};

static POSTGRES_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn set_postgres_turn_max_attempts(url: &str, turn_id: TurnId, max_attempts: i32) {
    assert!(max_attempts > 0);
    let connection = Database::connect(url).await.unwrap();
    let updated = connection
        .execute_unprepared(&format!(
            "UPDATE turn_run SET max_attempts = {max_attempts} WHERE id = '{}'",
            turn_id.0
        ))
        .await
        .unwrap();
    assert_eq!(updated.rows_affected(), 1);
}

#[allow(clippy::too_many_arguments)]
async fn park_wait_set_for_test<S: Store + ?Sized>(
    store: &S,
    wait_id: CallId,
    turn_id: TurnId,
    child_run_ids: &[AgentRunId],
    condition: AgentRunWaitCondition,
    lease_token: uuid::Uuid,
    expected_steer_revision: i64,
    progress: TurnCheckpointProgress,
    now: chrono::DateTime<Utc>,
) -> openwave_core::Result<Option<ParkTurnForAgentRunWaitSetOutcome>> {
    let turn = store
        .get_turn_run(turn_id)
        .await?
        .ok_or_else(|| openwave_core::AgentError::Store("test turn disappeared".into()))?;
    store
        .append_turn_event(
            turn.chat_id,
            turn_id,
            lease_token,
            1,
            now,
            &AgentEvent::TurnStarted { turn_id },
        )
        .await?;
    store
        .park_turn_for_agent_run_wait_set(
            &AgentRunWaitSetCheckpointRequest {
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

#[tokio::test]
async fn postgres_terminal_citations_are_atomic_and_exactly_recoverable() {
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
    let nanosecond_created_at =
        chrono::DateTime::<Utc>::from_timestamp(1_810_000_000, 123_456_789).unwrap();
    let standalone = Message {
        id: MessageId::new(),
        chat_id: chat.id,
        turn_id: TurnId::new(),
        role: Role::Assistant,
        content: "standalone".into(),
        created_at: nanosecond_created_at,
    };
    store
        .append_assistant_message_with_citations(&standalone, &[])
        .await
        .unwrap();
    store
        .append_assistant_message_with_citations(&standalone, &[])
        .await
        .unwrap();
    let turn_id = TurnId::new();
    assert!(matches!(
        store
            .accept_turn(turn_id, chat.id, "gpt-5", "cite")
            .await
            .unwrap(),
        AcceptTurnOutcome::Accepted(_)
    ));
    let claimed_at = utc_now_at_postgres_precision();
    let lease = uuid::Uuid::new_v4();
    store
        .claim_turn_run(lease, claimed_at, claimed_at + Duration::minutes(1))
        .await
        .unwrap();
    let document_id = DocumentId::new();
    store
        .upsert_document(&DocumentUpsert {
            id: document_id,
            project_id: None,
            chat_id: Some(chat.id),
            source_uri: None,
            media_type: "text/plain".into(),
            title: None,
            canonical_text: "fact".into(),
            updated_at: claimed_at,
        })
        .await
        .unwrap();
    let completed_at = utc_now_at_postgres_precision();
    let output = Message {
        id: MessageId::new(),
        chat_id: chat.id,
        turn_id,
        role: Role::Assistant,
        content: format!(":cit[answer]{{doc={} lines=1-1}}", document_id.0),
        created_at: completed_at,
    };
    let citation = AssistantCitationInput {
        document_id,
        locator: CitationLocator::Lines { start: 1, end: 1 },
    };
    assert!(matches!(
        store
            .complete_turn_run_with_citations_and_append_event(
                turn_id,
                lease,
                0,
                completed_at,
                &output,
                std::slice::from_ref(&citation),
                Usage::default(),
                StopReason::EndTurn,
            )
            .await
            .unwrap()
            .unwrap()
            .outcome,
        CompleteTurnRunOutcome::Completed(_)
    ));
    assert!(matches!(
        store
            .complete_turn_run_with_citations_and_append_event(
                turn_id,
                lease,
                0,
                utc_now_at_postgres_precision(),
                &output,
                std::slice::from_ref(&citation),
                Usage::default(),
                StopReason::EndTurn,
            )
            .await
            .unwrap()
            .unwrap()
            .outcome,
        CompleteTurnRunOutcome::Existing(_)
    ));
    assert!(store
        .complete_turn_run_with_citations_and_append_event(
            turn_id,
            lease,
            0,
            utc_now_at_postgres_precision(),
            &output,
            &[],
            Usage::default(),
            StopReason::EndTurn,
        )
        .await
        .is_err());
    assert_eq!(
        store
            .get_chat_transcript(chat.id)
            .await
            .unwrap()
            .unwrap()
            .citations
            .len(),
        1
    );
}

fn utc_now_at_postgres_precision() -> chrono::DateTime<Utc> {
    chrono::DateTime::<Utc>::from_timestamp_micros(Utc::now().timestamp_micros()).unwrap()
}

fn sample_chat() -> Chat {
    Chat {
        id: ChatId::new(),
        project_id: None,
        title: None,
        model: None,
        reasoning_effort: None,
        permission_mode: None,
        network_policy: Default::default(),
        attachment_revision: 0,
        root_attachments: Vec::new(),
        created_at: utc_now_at_postgres_precision(),
    }
}

async fn postgres_claim_exact_turn(
    store: &DbStore,
    target: TurnId,
    now: chrono::DateTime<Utc>,
) -> (TurnRun, uuid::Uuid) {
    for offset in 0..64 {
        let claim_at = now + Duration::milliseconds(offset);
        let lease = uuid::Uuid::new_v4();
        let outcome = store
            .claim_turn_run(lease, claim_at, claim_at + Duration::hours(1))
            .await
            .unwrap();
        let Some(turn) = outcome.turn else {
            continue;
        };
        if turn.id == target {
            return (turn, lease);
        }

        // PostgreSQL tests share an isolated database and a few older fixtures
        // intentionally leave queued work behind. Retire such stale fixtures so
        // this test can prove which exact continuation was made claimable.
        let cancel_at = claim_at + Duration::seconds(1);
        let cancellation = store
            .request_turn_cancellation(turn.id, cancel_at)
            .await
            .unwrap()
            .expect("claimed stale fixture remains cancellable");
        if matches!(cancellation, RequestTurnCancellationOutcome::Requested(_)) {
            store
                .finish_turn_cancellation(turn.id, lease, cancel_at + Duration::seconds(1))
                .await
                .unwrap()
                .expect("claimed stale fixture cancellation remains owned");
        }
    }
    panic!("target PostgreSQL turn {target} was not claimable");
}

async fn postgres_live_turn(
    store: &DbStore,
    chat_id: ChatId,
) -> (openwave_core::TurnRun, uuid::Uuid) {
    if let Some(turn) = store
        .list_turn_runs(chat_id)
        .await
        .unwrap()
        .into_iter()
        .find(|turn| turn.status == TurnRunStatus::Running)
    {
        return (
            turn.clone(),
            turn.lease_token.expect("running turn has lease"),
        );
    }
    let turn_id = TurnId::new();
    store
        .accept_turn(
            turn_id,
            chat_id,
            "postgres-sandbox-test",
            "sandbox admission",
        )
        .await
        .unwrap();
    let lease = uuid::Uuid::new_v4();
    let now = utc_now_at_postgres_precision();
    let turn = store
        .claim_turn_run(lease, now, now + Duration::hours(1))
        .await
        .unwrap()
        .turn
        .expect("postgres sandbox test turn should claim");
    (turn, lease)
}

async fn postgres_admit_sandbox(
    store: &DbStore,
    chat_id: ChatId,
    call: CallId,
    input: &str,
) -> openwave_core::AgentRun {
    let (turn, lease) = postgres_live_turn(store, chat_id).await;
    match store
        .admit_sandbox_agent_run(
            turn.id,
            call,
            input,
            lease,
            turn.steer_revision,
            openwave_core::AgentRun::MAX_CONCURRENCY_LIMIT,
            utc_now_at_postgres_precision(),
        )
        .await
        .unwrap()
        .expect("postgres sandbox admission should resolve")
    {
        openwave_core::AdmitSandboxAgentRunOutcome::Accepted { child, .. }
        | openwave_core::AdmitSandboxAgentRunOutcome::Existing { child, .. } => child,
        outcome => panic!("unexpected postgres sandbox admission: {outcome:?}"),
    }
}

async fn postgres_complete_next_child(store: &DbStore, text: &str) -> AgentRunId {
    let lease = uuid::Uuid::new_v4();
    let child = store
        .claim_agent_run(lease, Duration::minutes(5), 4, 4)
        .await
        .unwrap()
        .expect("postgres sandbox child should claim");
    match store
        .submit_agent_run_result(child.id, lease, text)
        .await
        .unwrap()
        .expect("postgres sandbox result should commit")
    {
        SubmitAgentRunResultOutcome::Completed(result) => assert_eq!(result.text, text),
        outcome => panic!("unexpected postgres sandbox submission: {outcome:?}"),
    }
    child.id
}

async fn cleanup_postgres_sandbox_chat(store: &DbStore, chat_id: ChatId) {
    for run in store
        .list_agent_runs(chat_id)
        .await
        .unwrap()
        .into_iter()
        .filter(|run| run.tier == AgentRunTier::Background)
    {
        let lease_token = run.lease_token;
        store.request_agent_run_cancellation(run.id).await.unwrap();
        let current = store.get_agent_run(run.id).await.unwrap().unwrap();
        if current.status == AgentRunStatus::Cancelling {
            store
                .finish_agent_run_cancellation(
                    run.id,
                    lease_token.expect("running sandbox cleanup retains its lease token"),
                )
                .await
                .unwrap()
                .expect("sandbox cleanup cancellation should resolve");
        }
    }

    for turn in store.list_turn_runs(chat_id).await.unwrap() {
        let lease_token = turn.lease_token;
        let cancel_at = utc_now_at_postgres_precision().max(turn.updated_at);
        store
            .request_turn_cancellation(turn.id, cancel_at)
            .await
            .unwrap();
        let current = store.get_turn_run(turn.id).await.unwrap().unwrap();
        if current.status == TurnRunStatus::Cancelling {
            let finish_at = utc_now_at_postgres_precision().max(current.updated_at);
            store
                .finish_turn_cancellation(
                    turn.id,
                    lease_token.expect("running turn cleanup retains its lease token"),
                    finish_at,
                )
                .await
                .unwrap()
                .expect("foreground cleanup cancellation should resolve");
        }
    }

    assert_eq!(
        store.delete_chat(chat_id).await.unwrap(),
        DeleteChatOutcome::Deleted
    );
}

fn postgres_spawn_checkpoint_request(
    turn: &openwave_core::TurnRun,
    lease_token: uuid::Uuid,
    call_id: CallId,
    task: &str,
) -> SandboxSpawnCheckpointRequest {
    SandboxSpawnCheckpointRequest {
        origin_turn_id: turn.id,
        lease_token,
        expected_steer_revision: turn.steer_revision,
        call_id,
        provider_id: format!("postgres-{call_id}"),
        arguments: serde_json::json!({"task": task}),
        result: serde_json::to_string(&SpawnSandboxAgentResult {
            agent_id: AgentRunId::sandbox_for_spawn_call(call_id),
        })
        .unwrap(),
        event_ordinal: 2,
        progress: TurnCheckpointProgress {
            model_steps: 1,
            usage: Usage {
                input_tokens: 5,
                output_tokens: 3,
                cache_read_input_tokens: 2,
                cache_creation_input_tokens: 1,
            },
        },
        max_active_background_agents: AgentRun::DEFAULT_MAX_ACTIVE_BACKGROUND_AGENTS,
        execution_location: openwave_core::AgentRunExecutionLocation::InProcess,
        approval_gated: false,
    }
}

async fn append_postgres_turn_started(
    store: &DbStore,
    turn: &openwave_core::TurnRun,
    lease: uuid::Uuid,
) {
    store
        .append_turn_event(
            turn.chat_id,
            turn.id,
            lease,
            1,
            utc_now_at_postgres_precision(),
            &AgentEvent::TurnStarted { turn_id: turn.id },
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn postgres_concurrent_exact_nonblocking_spawn_checkpoint_converges_once() {
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
    let (turn, lease) = postgres_live_turn(store.as_ref(), chat.id).await;
    append_postgres_turn_started(store.as_ref(), &turn, lease).await;
    let request = postgres_spawn_checkpoint_request(&turn, lease, CallId::new(), "race exactly");
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let mut tasks = Vec::new();
    for _ in 0..2 {
        let store = Arc::clone(&store);
        let barrier = Arc::clone(&barrier);
        let request = request.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            store
                .checkpoint_sandbox_spawn(&request, utc_now_at_postgres_precision())
                .await
                .unwrap()
                .unwrap()
        }));
    }
    let mut committed = 0;
    let mut existing = 0;
    for task in tasks {
        match task.await.unwrap() {
            CheckpointSandboxSpawnOutcome::Checkpointed { .. } => committed += 1,
            CheckpointSandboxSpawnOutcome::Existing { .. } => existing += 1,
            outcome => panic!("unexpected concurrent postgres checkpoint: {outcome:?}"),
        }
    }
    assert_eq!((committed, existing), (1, 1));
    assert_eq!(store.list_agent_runs(chat.id).await.unwrap().len(), 2);
    assert_eq!(store.list_tool_calls(chat.id).await.unwrap().len(), 1);
    cleanup_postgres_sandbox_chat(store.as_ref(), chat.id).await;
}

#[tokio::test]
async fn postgres_cross_chat_spawn_call_collision_has_one_atomic_winner() {
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
    let (first_turn, first_lease) = postgres_live_turn(store.as_ref(), first_chat.id).await;
    let (second_turn, second_lease) = postgres_live_turn(store.as_ref(), second_chat.id).await;
    append_postgres_turn_started(store.as_ref(), &first_turn, first_lease).await;
    append_postgres_turn_started(store.as_ref(), &second_turn, second_lease).await;
    let call_id = CallId::new();
    let requests = [
        postgres_spawn_checkpoint_request(&first_turn, first_lease, call_id, "first owner"),
        postgres_spawn_checkpoint_request(&second_turn, second_lease, call_id, "second owner"),
    ];
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let mut tasks = Vec::new();
    for request in requests {
        let store = Arc::clone(&store);
        let barrier = Arc::clone(&barrier);
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            store
                .checkpoint_sandbox_spawn(&request, utc_now_at_postgres_precision())
                .await
                .unwrap()
                .unwrap()
        }));
    }
    let outcomes = futures::future::join_all(tasks)
        .await
        .into_iter()
        .map(Result::unwrap)
        .collect::<Vec<_>>();
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, CheckpointSandboxSpawnOutcome::Checkpointed { .. }))
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, CheckpointSandboxSpawnOutcome::IdentityConflict))
            .count(),
        1
    );
    assert_eq!(
        store.list_tool_calls(first_chat.id).await.unwrap().len()
            + store.list_tool_calls(second_chat.id).await.unwrap().len(),
        1
    );
    cleanup_postgres_sandbox_chat(store.as_ref(), first_chat.id).await;
    cleanup_postgres_sandbox_chat(store.as_ref(), second_chat.id).await;
}

#[tokio::test]
async fn postgres_nonblocking_spawn_and_parent_cancellation_are_one_ordered_transition() {
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
    let (turn, lease) = postgres_live_turn(store.as_ref(), chat.id).await;
    append_postgres_turn_started(store.as_ref(), &turn, lease).await;
    let request =
        postgres_spawn_checkpoint_request(&turn, lease, CallId::new(), "race cancellation");
    let barrier = Arc::new(tokio::sync::Barrier::new(2));

    let checkpoint_task = {
        let store = Arc::clone(&store);
        let barrier = Arc::clone(&barrier);
        tokio::spawn(async move {
            barrier.wait().await;
            store
                .checkpoint_sandbox_spawn(&request, utc_now_at_postgres_precision())
                .await
                .unwrap()
                .unwrap()
        })
    };
    let cancellation_task = {
        let store = Arc::clone(&store);
        let barrier = Arc::clone(&barrier);
        tokio::spawn(async move {
            barrier.wait().await;
            store
                .request_turn_cancellation(turn.id, utc_now_at_postgres_precision())
                .await
                .unwrap()
        })
    };
    let checkpoint = checkpoint_task.await.unwrap();
    let cancellation = cancellation_task.await.unwrap();
    let parent = store.get_turn_run(turn.id).await.unwrap().unwrap();
    let children = store
        .list_agent_runs(chat.id)
        .await
        .unwrap()
        .into_iter()
        .filter(|run| run.tier == AgentRunTier::Background)
        .collect::<Vec<_>>();
    match (checkpoint, cancellation) {
        (
            CheckpointSandboxSpawnOutcome::Checkpointed { .. },
            Some(RequestTurnCancellationOutcome::Cancelled(_)),
        ) => {
            assert_eq!(parent.status, TurnRunStatus::Cancelled);
            assert_eq!(children.len(), 1);
            assert_eq!(children[0].status, AgentRunStatus::Cancelled);
            assert_eq!(store.list_tool_calls(chat.id).await.unwrap().len(), 1);
        }
        // Cancellation timestamps are mutation fences. If the checkpoint
        // commits after the request timestamp was captured, rejecting that
        // stale request is correct even though it wins the scheduler lock
        // second.
        (CheckpointSandboxSpawnOutcome::Checkpointed { .. }, None) => {
            assert_eq!(parent.status, TurnRunStatus::Resuming);
            assert_eq!(children.len(), 1);
            assert_eq!(children[0].status, AgentRunStatus::Queued);
            assert_eq!(store.list_tool_calls(chat.id).await.unwrap().len(), 1);
        }
        (
            CheckpointSandboxSpawnOutcome::LeaseLost,
            Some(RequestTurnCancellationOutcome::Requested(_)),
        ) => {
            assert_eq!(parent.status, TurnRunStatus::Cancelling);
            assert!(children.is_empty());
            assert!(store.list_tool_calls(chat.id).await.unwrap().is_empty());
        }
        outcome => panic!("unexpected checkpoint/cancellation race outcome: {outcome:?}"),
    }
    cleanup_postgres_sandbox_chat(store.as_ref(), chat.id).await;
}

#[tokio::test]
async fn postgres_parent_cancellation_and_first_child_claim_converge_without_identity_errors() {
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
    let (origin, origin_lease) = postgres_live_turn(&store, chat.id).await;
    let child = postgres_admit_sandbox(
        &store,
        chat.id,
        CallId::new(),
        "race the first claim against parent cancellation",
    )
    .await;
    let child_lease = uuid::Uuid::new_v4();
    let barrier = Arc::new(tokio::sync::Barrier::new(2));

    let claim = {
        let store = store.clone();
        let barrier = barrier.clone();
        tokio::spawn(async move {
            barrier.wait().await;
            store
                .claim_agent_run(child_lease, Duration::minutes(5), 4, 4)
                .await
        })
    };
    let cancel = {
        let store = store.clone();
        let barrier = barrier.clone();
        tokio::spawn(async move {
            barrier.wait().await;
            store
                .request_turn_cancellation(
                    origin.id,
                    utc_now_at_postgres_precision().max(origin.updated_at),
                )
                .await
        })
    };
    let claimed = claim
        .await
        .unwrap()
        .expect("claim race must not return Store error");
    let cancelled = cancel
        .await
        .unwrap()
        .expect("parent cancellation race must not return Store error");
    assert!(cancelled.is_some());
    let claim_won = claimed.is_some();
    if let Some(claimed) = claimed.as_ref() {
        assert_eq!(claimed.id, child.id);
    }

    let current = store.get_agent_run(child.id).await.unwrap().unwrap();
    match current.status {
        AgentRunStatus::Cancelled => {
            assert!(!claim_won, "cancelled child cannot also commit a claim");
        }
        AgentRunStatus::Cancelling => {
            assert_eq!(current.lease_token, Some(child_lease));
            assert!(matches!(
                store
                    .finish_agent_run_cancellation(child.id, child_lease)
                    .await
                    .unwrap(),
                Some(FinishAgentRunCancellationOutcome::Cancelled(_))
            ));
        }
        status => panic!("unexpected child state after claim/cancel race: {status:?}"),
    }
    let inbox = store
        .list_agent_run_inbox(AgentRunId::foreground_for_chat(chat.id))
        .await
        .unwrap();
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].status, AgentRunInboxStatus::Cancelled);
    assert!(matches!(
        inbox[0].result.payload,
        AgentRunResultPayload::Cancelled {
            reason: AgentRunCancellationReason::ParentTurnCancelled
        }
    ));
    let parent = store.get_turn_run(origin.id).await.unwrap().unwrap();
    if parent.status == TurnRunStatus::Cancelling {
        store
            .finish_turn_cancellation(
                origin.id,
                origin_lease,
                utc_now_at_postgres_precision().max(parent.updated_at),
            )
            .await
            .unwrap()
            .unwrap();
    }
    cleanup_postgres_sandbox_chat(&store, chat.id).await;

    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (origin, origin_lease) = postgres_live_turn(&store, chat.id).await;
    let child = postgres_admit_sandbox(
        &store,
        chat.id,
        CallId::new(),
        "race direct cancellation against parent cancellation",
    )
    .await;
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let direct = {
        let store = store.clone();
        let barrier = barrier.clone();
        tokio::spawn(async move {
            barrier.wait().await;
            store.request_agent_run_cancellation(child.id).await
        })
    };
    let parent = {
        let store = store.clone();
        let barrier = barrier.clone();
        tokio::spawn(async move {
            barrier.wait().await;
            store
                .request_turn_cancellation(
                    origin.id,
                    utc_now_at_postgres_precision().max(origin.updated_at),
                )
                .await
        })
    };
    assert!(direct
        .await
        .unwrap()
        .expect("direct cancellation race must not return Store error")
        .is_some());
    assert!(parent
        .await
        .unwrap()
        .expect("parent cancellation race must not return Store error")
        .is_some());
    let inbox = store
        .list_agent_run_inbox(AgentRunId::foreground_for_chat(chat.id))
        .await
        .unwrap();
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].status, AgentRunInboxStatus::Cancelled);
    assert!(matches!(
        inbox[0].result.payload,
        AgentRunResultPayload::Cancelled {
            reason: AgentRunCancellationReason::Requested
                | AgentRunCancellationReason::ParentTurnCancelled
        }
    ));
    assert!(matches!(
        store
            .request_agent_run_cancellation(child.id)
            .await
            .unwrap(),
        Some(RequestAgentRunCancellationOutcome::Existing(_))
    ));
    let parent = store.get_turn_run(origin.id).await.unwrap().unwrap();
    if parent.status == TurnRunStatus::Cancelling {
        store
            .finish_turn_cancellation(
                origin.id,
                origin_lease,
                utc_now_at_postgres_precision().max(parent.updated_at),
            )
            .await
            .unwrap()
            .unwrap();
    }
    cleanup_postgres_sandbox_chat(&store, chat.id).await;
}

#[tokio::test]
async fn postgres_parent_completion_and_child_admission_form_one_terminal_boundary() {
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
    let (turn, lease) = postgres_live_turn(store.as_ref(), chat.id).await;
    let call = CallId::new();
    let output = Message {
        id: MessageId::new(),
        chat_id: chat.id,
        turn_id: turn.id,
        role: Role::Assistant,
        content: "race-safe terminal answer".into(),
        created_at: utc_now_at_postgres_precision().max(turn.updated_at),
    };
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let admission = {
        let store = Arc::clone(&store);
        let barrier = Arc::clone(&barrier);
        tokio::spawn(async move {
            barrier.wait().await;
            store
                .admit_sandbox_agent_run(
                    turn.id,
                    call,
                    "race parent completion",
                    lease,
                    turn.steer_revision,
                    openwave_core::AgentRun::MAX_CONCURRENCY_LIMIT,
                    utc_now_at_postgres_precision(),
                )
                .await
                .unwrap()
                .unwrap()
        })
    };
    let completion = {
        let store = Arc::clone(&store);
        let barrier = Arc::clone(&barrier);
        tokio::spawn(async move {
            barrier.wait().await;
            store
                .complete_turn_run(
                    turn.id,
                    lease,
                    turn.steer_revision,
                    utc_now_at_postgres_precision(),
                    &output,
                )
                .await
                .unwrap()
                .unwrap()
        })
    };
    let admission = admission.await.unwrap();
    let completion = completion.await.unwrap();
    let parent = store.get_turn_run(turn.id).await.unwrap().unwrap();
    let children = store
        .list_agent_runs(chat.id)
        .await
        .unwrap()
        .into_iter()
        .filter(|run| run.tier == AgentRunTier::Background)
        .collect::<Vec<_>>();
    match completion {
        CompleteTurnRunOutcome::Completed(_) => {
            assert!(matches!(
                admission,
                openwave_core::AdmitSandboxAgentRunOutcome::ParentUnavailable
                    | openwave_core::AdmitSandboxAgentRunOutcome::LeaseLost
            ));
            assert_eq!(parent.status, TurnRunStatus::Completed);
            assert!(children.is_empty());
        }
        CompleteTurnRunOutcome::ChildrenOutstanding { child_run_ids, .. } => {
            assert!(matches!(
                admission,
                openwave_core::AdmitSandboxAgentRunOutcome::Accepted { .. }
            ));
            assert_eq!(parent.status, TurnRunStatus::Running);
            assert_eq!(children.len(), 1);
            assert_eq!(child_run_ids, vec![children[0].id]);
        }
        outcome => panic!("unexpected completion/admission race outcome: {outcome:?}"),
    }
    cleanup_postgres_sandbox_chat(store.as_ref(), chat.id).await;
}

#[tokio::test]
async fn postgres_permanent_parent_failure_and_child_result_cannot_orphan_delivery() {
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
    let (turn, turn_lease) = postgres_live_turn(store.as_ref(), chat.id).await;
    let child = postgres_admit_sandbox(
        store.as_ref(),
        chat.id,
        CallId::new(),
        "race parent failure",
    )
    .await;
    let child_lease = uuid::Uuid::new_v4();
    assert_eq!(
        store
            .claim_agent_run(child_lease, Duration::minutes(5), 4, 4)
            .await
            .unwrap()
            .unwrap()
            .id,
        child.id
    );
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let submission = {
        let store = Arc::clone(&store);
        let barrier = Arc::clone(&barrier);
        tokio::spawn(async move {
            barrier.wait().await;
            store
                .submit_agent_run_result(child.id, child_lease, "terminal child result")
                .await
                .unwrap()
        })
    };
    let failure = {
        let store = Arc::clone(&store);
        let barrier = Arc::clone(&barrier);
        tokio::spawn(async move {
            barrier.wait().await;
            store
                .record_turn_run_failure(
                    turn.id,
                    turn_lease,
                    utc_now_at_postgres_precision(),
                    TurnFailureRetry::Permanent,
                    0,
                    Usage::default(),
                    "provider_failed",
                    Some("provider failed permanently"),
                )
                .await
                .unwrap()
                .unwrap()
        })
    };
    let submission = submission.await.unwrap();
    assert!(matches!(
        failure.await.unwrap(),
        RecordTurnFailureOutcome::Recorded(_)
    ));
    let parent = store.get_turn_run(turn.id).await.unwrap().unwrap();
    let child = store.get_agent_run(child.id).await.unwrap().unwrap();
    assert_eq!(parent.status, TurnRunStatus::Failed);
    match child.status {
        AgentRunStatus::Completed => {
            assert!(matches!(
                submission,
                Some(SubmitAgentRunResultOutcome::Completed(_))
            ));
            let inbox = store
                .list_agent_run_inbox(AgentRunId::foreground_for_chat(chat.id))
                .await
                .unwrap();
            assert_eq!(inbox.len(), 1);
            assert_eq!(inbox[0].status, AgentRunInboxStatus::Cancelled);
        }
        AgentRunStatus::Cancelling => {
            assert!(submission.is_none());
            store
                .finish_agent_run_cancellation(child.id, child_lease)
                .await
                .unwrap()
                .unwrap();
            let inbox = store
                .list_agent_run_inbox(AgentRunId::foreground_for_chat(chat.id))
                .await
                .unwrap();
            assert!(matches!(
                inbox[0].result.payload,
                AgentRunResultPayload::Cancelled {
                    reason: AgentRunCancellationReason::ParentTurnFailed
                }
            ));
            assert_eq!(inbox[0].status, AgentRunInboxStatus::Cancelled);
        }
        status => panic!("unexpected child status after permanent parent failure: {status:?}"),
    }
    cleanup_postgres_sandbox_chat(store.as_ref(), chat.id).await;
}

#[tokio::test]
async fn postgres_parent_cancellation_uses_time_after_admission_and_heartbeat_lock_waits() {
    let _guard = POSTGRES_TEST_LOCK.lock().await;
    let url = match std::env::var("OPENWAVE_POSTGRES_TEST_URL") {
        Ok(url) => url,
        Err(_) if std::env::var_os("OPENWAVE_REQUIRE_POSTGRES_TEST").is_some() => {
            panic!("OPENWAVE_POSTGRES_TEST_URL must name an isolated test database")
        }
        Err(_) => return,
    };
    let store = DbStore::connect(&url).await.unwrap();

    // An admission can already own chat/turn while cancellation owns the
    // scheduler and waits for chat. Commit a correctly shaped child from that
    // transaction after the caller timestamp was captured; cancellation must
    // use post-lock database time for the child result and parent transition.
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (origin, origin_lease) = postgres_live_turn(&store, chat.id).await;
    let call = CallId::new();
    let child_id = AgentRunId::sandbox_for_spawn_call(call);
    let blocker_connection = Database::connect(&url).await.unwrap();
    let blocker = blocker_connection.begin().await.unwrap();
    blocker
        .execute_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            format!("UPDATE chat SET title = title WHERE id = '{}'", chat.id.0),
        ))
        .await
        .unwrap();
    let stale_now = utc_now_at_postgres_precision();
    let contender = store.clone();
    let cancellation = tokio::spawn(async move {
        contender
            .request_turn_cancellation(origin.id, stale_now)
            .await
    });
    tokio::time::sleep(StdDuration::from_millis(100)).await;
    blocker
        .execute_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            format!(
                "INSERT INTO agent_run (id, chat_id, parent_id, parent_depth, spawn_call_id, tier, execution_location, depth, status, input, attempt_count, max_attempts, claim_count, available_at, deadline_at, lease_token, lease_expires_at, started_at, finished_at, last_error_code, last_error_detail, created_at, updated_at) \
                 SELECT '{}', '{}', '{}', 0, '{}', 'background', 'in_process', 1, 'queued', 'admitted while cancellation waited', 0, 3, 0, admitted_at, admitted_at + interval '30 minutes', NULL, NULL, NULL, NULL, NULL, NULL, admitted_at, admitted_at \
                 FROM (SELECT clock_timestamp() AS admitted_at) AS admission_clock",
                child_id.0,
                chat.id.0,
                AgentRunId::foreground_for_chat(chat.id).0,
                call.0,
            ),
        ))
        .await
        .unwrap();
    blocker
        .execute_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            format!(
                "INSERT INTO sandbox_agent_admission (child_run_id, parent_run_id, origin_turn_id, chat_id, spawn_call_id, admitted_at) \
                 VALUES ('{}', '{}', '{}', '{}', '{}', clock_timestamp())",
                child_id.0,
                AgentRunId::foreground_for_chat(chat.id).0,
                origin.id.0,
                chat.id.0,
                call.0,
            ),
        ))
        .await
        .unwrap();
    blocker.commit().await.unwrap();
    assert!(cancellation
        .await
        .unwrap()
        .expect("post-admission cancellation must not regress time")
        .is_some());
    let child = store.get_agent_run(child_id).await.unwrap().unwrap();
    assert_eq!(child.status, AgentRunStatus::Cancelled);
    assert!(child.updated_at >= child.created_at);
    let parent = store.get_turn_run(origin.id).await.unwrap().unwrap();
    store
        .finish_turn_cancellation(
            origin.id,
            origin_lease,
            utc_now_at_postgres_precision().max(parent.updated_at),
        )
        .await
        .unwrap()
        .unwrap();
    cleanup_postgres_sandbox_chat(&store, chat.id).await;

    // A heartbeat owns the scheduler row. Cancellation captures its caller
    // time before waiting, then must advance from the heartbeat's database
    // timestamp rather than overwrite it with stale request time.
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (origin, origin_lease) = postgres_live_turn(&store, chat.id).await;
    let child = postgres_admit_sandbox(
        &store,
        chat.id,
        CallId::new(),
        "heartbeat while parent cancellation waits",
    )
    .await;
    let child_lease = uuid::Uuid::new_v4();
    store
        .claim_agent_run(child_lease, Duration::minutes(5), 4, 4)
        .await
        .unwrap()
        .unwrap();
    let blocker_connection = Database::connect(&url).await.unwrap();
    let blocker = blocker_connection.begin().await.unwrap();
    blocker
        .execute_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            "UPDATE agent_run_claim_lock SET id = id WHERE id = 1",
        ))
        .await
        .unwrap();
    let stale_now = utc_now_at_postgres_precision();
    let contender = store.clone();
    let cancellation = tokio::spawn(async move {
        contender
            .request_turn_cancellation(origin.id, stale_now)
            .await
    });
    tokio::time::sleep(StdDuration::from_millis(100)).await;
    blocker
        .execute_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            format!(
                "UPDATE agent_run SET lease_expires_at = clock_timestamp() + interval '5 minutes', updated_at = clock_timestamp() WHERE id = '{}'",
                child.id.0
            ),
        ))
        .await
        .unwrap();
    let heartbeat_row = blocker
        .query_one_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            format!(
                "SELECT updated_at FROM agent_run WHERE id = '{}'",
                child.id.0
            ),
        ))
        .await
        .unwrap()
        .unwrap();
    let heartbeat_at = heartbeat_row
        .try_get::<chrono::DateTime<Utc>>("", "updated_at")
        .unwrap();
    blocker.commit().await.unwrap();
    assert!(cancellation
        .await
        .unwrap()
        .expect("post-heartbeat cancellation must not regress time")
        .is_some());
    let child = store.get_agent_run(child.id).await.unwrap().unwrap();
    assert_eq!(child.status, AgentRunStatus::Cancelling);
    assert!(child.updated_at >= heartbeat_at);
    store
        .finish_agent_run_cancellation(child.id, child_lease)
        .await
        .unwrap()
        .unwrap();
    let parent = store.get_turn_run(origin.id).await.unwrap().unwrap();
    store
        .finish_turn_cancellation(
            origin.id,
            origin_lease,
            utc_now_at_postgres_precision().max(parent.updated_at),
        )
        .await
        .unwrap()
        .unwrap();
    cleanup_postgres_sandbox_chat(&store, chat.id).await;
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

    let spawn_call_id = CallId::new();
    let child = postgres_admit_sandbox(&store, chat.id, spawn_call_id, "postgres child").await;
    let child_id = child.id;
    assert_eq!(child.depth, 1);
    assert_eq!(child.status, AgentRunStatus::Queued);
    assert_eq!(
        postgres_admit_sandbox(&store, chat.id, spawn_call_id, "postgres child").await,
        child
    );
    assert!(store
        .accept_agent_run(
            AgentRunId::new(),
            chat.id,
            Some(child_id),
            Some(CallId::new()),
            AgentRunTier::Background,
            Some("forbidden grandchild"),
        )
        .await
        .is_err());
    cleanup_postgres_sandbox_chat(&store, chat.id).await;
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
    let (first_turn, first_lease) = postgres_live_turn(&store, first_chat.id).await;
    let (second_turn, second_lease) = postgres_live_turn(&store, second_chat.id).await;
    let collision_call = CallId::new();
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let mut tasks = Vec::new();
    for (turn, lease) in [(first_turn, first_lease), (second_turn, second_lease)] {
        let store = store.clone();
        let barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            store
                .admit_sandbox_agent_run(
                    turn.id,
                    collision_call,
                    "colliding child",
                    lease,
                    turn.steer_revision,
                    1,
                    utc_now_at_postgres_precision(),
                )
                .await
        }));
    }
    let mut accepted = 0;
    let mut conflicted = 0;
    for task in tasks {
        match task.await.unwrap() {
            Ok(Some(openwave_core::AdmitSandboxAgentRunOutcome::Accepted { .. })) => accepted += 1,
            Ok(Some(openwave_core::AdmitSandboxAgentRunOutcome::IdentityConflict)) => {
                conflicted += 1
            }
            outcome => panic!("unexpected cross-chat collision outcome: {outcome:?}"),
        }
    }
    assert_eq!((accepted, conflicted), (1, 1));
    cleanup_postgres_sandbox_chat(&store, first_chat.id).await;
    cleanup_postgres_sandbox_chat(&store, second_chat.id).await;
}

#[tokio::test]
async fn postgres_concurrent_sandbox_admission_respects_origin_turn_cap() {
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
    let (turn, lease) = postgres_live_turn(&store, chat.id).await;
    let barrier = Arc::new(tokio::sync::Barrier::new(8));
    let mut tasks = Vec::new();
    for index in 0..8 {
        let contender = DbStore::connect(&url).await.unwrap();
        let barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            contender
                .admit_sandbox_agent_run(
                    turn.id,
                    CallId::new(),
                    &format!("postgres concurrent admission {index}"),
                    lease,
                    turn.steer_revision,
                    2,
                    utc_now_at_postgres_precision(),
                )
                .await
                .unwrap()
                .unwrap()
        }));
    }
    let outcomes = futures::future::join_all(tasks).await;
    let accepted = outcomes
        .into_iter()
        .map(Result::unwrap)
        .filter(|outcome| {
            matches!(
                outcome,
                openwave_core::AdmitSandboxAgentRunOutcome::Accepted { .. }
            )
        })
        .count();
    assert_eq!(accepted, 2);
    let sandbox_runs = store
        .list_agent_runs(chat.id)
        .await
        .unwrap()
        .into_iter()
        .filter(|run| run.tier == AgentRunTier::Background)
        .collect::<Vec<_>>();
    assert_eq!(sandbox_runs.len(), 2);
    cleanup_postgres_sandbox_chat(&store, chat.id).await;
}

#[tokio::test]
async fn postgres_concurrent_exact_sandbox_admission_converges() {
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
    let (turn, lease) = postgres_live_turn(&store, chat.id).await;
    let call = CallId::new();
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let mut tasks = Vec::new();
    for _ in 0..2 {
        let contender = DbStore::connect(&url).await.unwrap();
        let barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            contender
                .admit_sandbox_agent_run(
                    turn.id,
                    call,
                    "same postgres child",
                    lease,
                    turn.steer_revision,
                    2,
                    utc_now_at_postgres_precision(),
                )
                .await
                .unwrap()
                .unwrap()
        }));
    }
    let outcomes = futures::future::join_all(tasks).await;
    let accepted = outcomes
        .iter()
        .filter(|outcome| {
            matches!(
                outcome.as_ref().unwrap(),
                openwave_core::AdmitSandboxAgentRunOutcome::Accepted { .. }
            )
        })
        .count();
    let existing = outcomes
        .iter()
        .filter(|outcome| {
            matches!(
                outcome.as_ref().unwrap(),
                openwave_core::AdmitSandboxAgentRunOutcome::Existing { .. }
            )
        })
        .count();
    assert_eq!((accepted, existing), (1, 1));
    cleanup_postgres_sandbox_chat(&store, chat.id).await;
}

#[tokio::test]
async fn postgres_sandbox_admission_checks_lease_time_after_lock_wait() {
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
    let (turn, lease) = postgres_live_turn(&store, chat.id).await;
    let setup_connection = Database::connect(&url).await.unwrap();
    setup_connection
        .execute_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            format!(
                "UPDATE turn_run SET lease_expires_at = clock_timestamp() + interval '200 milliseconds' WHERE id = '{}'",
                turn.id.0
            ),
        ))
        .await
        .unwrap();

    let blocker_connection = Database::connect(&url).await.unwrap();
    let blocker = blocker_connection.begin().await.unwrap();
    blocker
        .execute_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            format!("UPDATE chat SET title = title WHERE id = '{}'", chat.id.0),
        ))
        .await
        .unwrap();

    let contender = store.clone();
    let call = CallId::new();
    let stale_caller_now = utc_now_at_postgres_precision();
    let admission = tokio::spawn(async move {
        contender
            .admit_sandbox_agent_run(
                turn.id,
                call,
                "must not cross an expired lease",
                lease,
                turn.steer_revision,
                2,
                stale_caller_now,
            )
            .await
    });
    tokio::time::sleep(StdDuration::from_millis(350)).await;
    blocker.commit().await.unwrap();

    assert!(matches!(
        admission.await.unwrap().unwrap(),
        Some(openwave_core::AdmitSandboxAgentRunOutcome::LeaseLost)
    ));
    assert!(store
        .get_agent_run(AgentRunId::sandbox_for_spawn_call(call))
        .await
        .unwrap()
        .is_none());
    cleanup_postgres_sandbox_chat(&store, chat.id).await;
}

#[tokio::test]
async fn postgres_multi_child_wait_resumes_in_request_order_exactly_once() {
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
    let (turn, lease) = postgres_live_turn(&store, chat.id).await;
    let first = postgres_admit_sandbox(&store, chat.id, CallId::new(), "postgres first").await;
    let second = postgres_admit_sandbox(&store, chat.id, CallId::new(), "postgres second").await;
    let requested = [second.id, first.id];
    let wait_id = CallId::new();
    let progress = TurnCheckpointProgress {
        model_steps: 1,
        usage: Usage {
            input_tokens: 3,
            output_tokens: 2,
            cache_read_input_tokens: 1,
            cache_creation_input_tokens: 0,
        },
    };
    assert!(matches!(
        park_wait_set_for_test(
            &store,
            wait_id,
            turn.id,
            &requested,
            AgentRunWaitCondition::All,
            lease,
            turn.steer_revision,
            progress,
            utc_now_at_postgres_precision(),
        )
        .await
        .unwrap(),
        Some(ParkTurnForAgentRunWaitSetOutcome::Parked { .. })
    ));
    postgres_complete_next_child(&store, "postgres completion one").await;
    assert!(store
        .list_ready_agent_run_wait_set_candidates(1)
        .await
        .unwrap()
        .is_empty());
    let resume_token = uuid::Uuid::new_v4();
    assert!(matches!(
        store
            .resume_turn_for_agent_run_wait_set(wait_id, resume_token)
            .await
            .unwrap(),
        Some(ResumeTurnForAgentRunWaitSetOutcome::NotReady(_))
    ));
    postgres_complete_next_child(&store, "postgres completion two").await;
    let candidate = store
        .list_ready_agent_run_wait_set_candidates(1)
        .await
        .unwrap()
        .into_iter()
        .next()
        .expect("postgres recovery scan should decode a ready wait");
    assert_eq!(candidate.wait_id, wait_id);
    assert_eq!(
        candidate.ready_at,
        store
            .list_agent_run_inbox(turn.agent_run_id)
            .await
            .unwrap()
            .into_iter()
            .map(|entry| entry.delivered_at)
            .max()
            .unwrap()
    );
    let results = match store
        .resume_turn_for_agent_run_wait_set(wait_id, resume_token)
        .await
        .unwrap()
        .expect("postgres all wait should resume")
    {
        ResumeTurnForAgentRunWaitSetOutcome::Resumed { results, .. } => results,
        outcome => panic!("unexpected postgres multi-wait resume: {outcome:?}"),
    };
    assert_eq!(
        results
            .iter()
            .map(|entry| entry.child_run_id)
            .collect::<Vec<_>>(),
        requested
    );
    assert!(results.iter().all(|entry| {
        entry.status == AgentRunInboxStatus::Consumed
            && entry.consumed_lease_token == Some(resume_token)
    }));
    assert!(matches!(
        store
            .resume_turn_for_agent_run_wait_set(wait_id, resume_token)
            .await
            .unwrap(),
        Some(ResumeTurnForAgentRunWaitSetOutcome::Existing { .. })
    ));
    cleanup_postgres_sandbox_chat(&store, chat.id).await;
}

#[tokio::test]
async fn postgres_concurrent_cross_chat_wait_identity_converges_to_typed_conflict() {
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
    let (first_turn, first_lease) = postgres_live_turn(&store, first_chat.id).await;
    let (second_turn, second_lease) = postgres_live_turn(&store, second_chat.id).await;
    let first_child = postgres_admit_sandbox(
        &store,
        first_chat.id,
        CallId::new(),
        "postgres collision first",
    )
    .await;
    let second_child = postgres_admit_sandbox(
        &store,
        second_chat.id,
        CallId::new(),
        "postgres collision second",
    )
    .await;
    let wait_id = CallId::new();
    let first_members = [first_child.id];
    let second_members = [second_child.id];
    let first_store = store.clone();
    let second_store = store.clone();
    let progress = TurnCheckpointProgress {
        model_steps: 1,
        usage: Usage::default(),
    };
    let (first, second) = tokio::join!(
        park_wait_set_for_test(
            &first_store,
            wait_id,
            first_turn.id,
            &first_members,
            AgentRunWaitCondition::All,
            first_lease,
            first_turn.steer_revision,
            progress,
            utc_now_at_postgres_precision(),
        ),
        park_wait_set_for_test(
            &second_store,
            wait_id,
            second_turn.id,
            &second_members,
            AgentRunWaitCondition::All,
            second_lease,
            second_turn.steer_revision,
            progress,
            utc_now_at_postgres_precision(),
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
    cleanup_postgres_sandbox_chat(&store, first_chat.id).await;
    cleanup_postgres_sandbox_chat(&store, second_chat.id).await;
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
        let _accepted = postgres_admit_sandbox(
            store.as_ref(),
            chat_id,
            CallId::new(),
            "postgres concurrent claim",
        )
        .await;
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
    cleanup_postgres_sandbox_chat(store.as_ref(), first_chat.id).await;
    cleanup_postgres_sandbox_chat(store.as_ref(), second_chat.id).await;
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
    let child_id = postgres_admit_sandbox(
        &store,
        chat.id,
        CallId::new(),
        "scheduler lock clock regression",
    )
    .await
    .id;

    let blocker = Database::connect(&url).await.unwrap();
    blocker
        .execute_raw(Statement::from_string(
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
        .execute_raw(Statement::from_string(
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
    let lock_released_at = Utc::now();
    transaction.commit().await.unwrap();

    let claimed = claim.await.unwrap();
    assert_eq!(claimed.id, child_id);
    // Compare with the lock-release boundary, not the time this task happens
    // to be polled again. A busy runner may resume this assertion after the
    // intentionally short lease has expired; the regression this test guards
    // is whether the lease was based on the pre-wait transaction timestamp.
    assert!(claimed.lease_expires_at.unwrap() > lock_released_at);
    // Restore a cancellable nonterminal shape before shared cleanup. Forging a
    // terminal child without its immutable result/inbox would now correctly
    // be treated as corruption by the parent terminal guard.
    blocker
        .execute_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            format!(
                "UPDATE agent_run \
                 SET status = 'queued', lease_token = NULL, lease_expires_at = NULL, \
                     finished_at = NULL, updated_at = clock_timestamp() \
                 WHERE id = '{}'",
                child_id.0
            ),
        ))
        .await
        .unwrap();
    cleanup_postgres_sandbox_chat(&store, chat.id).await;
}

#[tokio::test]
async fn postgres_sandbox_terminal_transitions_are_exact_and_fenced() {
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
    let parent_id = AgentRunId::foreground_for_chat(chat.id);

    let completed_id =
        postgres_admit_sandbox(&store, chat.id, CallId::new(), "postgres terminal result")
            .await
            .id;
    let prioritizer = Database::connect(&url).await.unwrap();
    prioritizer
        .execute_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            format!(
                "UPDATE agent_run \
                 SET created_at = TIMESTAMPTZ '2000-01-01 00:00:00+00', \
                     available_at = TIMESTAMPTZ '2000-01-01 00:00:00+00', \
                     updated_at = clock_timestamp() \
                 WHERE id = '{}'",
                completed_id.0
            ),
        ))
        .await
        .unwrap();
    let completed_token = uuid::Uuid::new_v4();
    store
        .claim_agent_run(completed_token, Duration::minutes(1), 1_024, 1_024)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        store
            .submit_agent_run_result(completed_id, completed_token, "postgres result")
            .await
            .unwrap(),
        Some(SubmitAgentRunResultOutcome::Completed(result))
            if result.agent_run_id == completed_id && result.text == "postgres result"
    ));
    let inbox = store.list_agent_run_inbox(parent_id).await.unwrap();
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].parent_run_id, parent_id);
    assert_eq!(inbox[0].child_run_id, completed_id);
    assert_eq!(inbox[0].result.text, "postgres result");
    assert!(matches!(
        store
            .submit_agent_run_result(completed_id, completed_token, "postgres result")
            .await
            .unwrap(),
        Some(SubmitAgentRunResultOutcome::Existing(_))
    ));

    let cancelling_id =
        postgres_admit_sandbox(&store, chat.id, CallId::new(), "postgres cancellation")
            .await
            .id;
    prioritizer
        .execute_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            format!(
                "UPDATE agent_run \
                 SET created_at = TIMESTAMPTZ '1999-01-01 00:00:00+00', \
                     available_at = TIMESTAMPTZ '1999-01-01 00:00:00+00', \
                     updated_at = clock_timestamp() \
                 WHERE id = '{}'",
                cancelling_id.0
            ),
        ))
        .await
        .unwrap();
    let cancelling_token = uuid::Uuid::new_v4();
    store
        .claim_agent_run(cancelling_token, Duration::minutes(1), 1_024, 1_024)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        store
            .request_agent_run_cancellation(cancelling_id)
            .await
            .unwrap(),
        Some(RequestAgentRunCancellationOutcome::Requested(run))
            if run.status == AgentRunStatus::Cancelling
    ));
    assert!(matches!(
        store
            .finish_agent_run_cancellation(cancelling_id, cancelling_token)
            .await
            .unwrap(),
        Some(FinishAgentRunCancellationOutcome::Cancelled(run))
            if run.status == AgentRunStatus::Cancelled
    ));
    cleanup_postgres_sandbox_chat(&store, chat.id).await;
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
async fn postgres_project_deletion_serializes_with_staged_source_ingestion() {
    let _guard = POSTGRES_TEST_LOCK.lock().await;
    let url = match std::env::var("OPENWAVE_POSTGRES_TEST_URL") {
        Ok(url) => url,
        Err(_) if std::env::var_os("OPENWAVE_REQUIRE_POSTGRES_TEST").is_some() => {
            panic!("OPENWAVE_POSTGRES_TEST_URL must name an isolated test database")
        }
        Err(_) => return,
    };
    let store = DbStore::connect(&url).await.unwrap();
    let project = Project {
        id: ProjectId::new(),
        title: Some("postgres deletion race".into()),
        attachment_revision: 0,
        root_attachments: Vec::new(),
        created_at: utc_now_at_postgres_precision(),
    };
    store.create_project(&project).await.unwrap();
    let source = DocumentSourceUpsert {
        id: DocumentId::new(),
        project_id: Some(project.id),
        chat_id: None,
        source_uri: Some("file:///postgres-project-race.bin".into()),
        media_type: "application/octet-stream".into(),
        title: None,
        source_blob: DocumentSourceBlob::from_digest([0x7a; 32], 1),
        canonical_text: String::new(),
        updated_at: utc_now_at_postgres_precision(),
    };

    let barrier = Arc::new(tokio::sync::Barrier::new(3));
    let delete_store = store.clone();
    let delete_barrier = barrier.clone();
    let delete = tokio::spawn(async move {
        delete_barrier.wait().await;
        delete_store.delete_project(project.id).await
    });
    let ingest_store = store.clone();
    let ingest_barrier = barrier.clone();
    let ingest_source = source.clone();
    let ingest = tokio::spawn(async move {
        ingest_barrier.wait().await;
        ingest_store.accept_document_source(&ingest_source).await
    });
    barrier.wait().await;

    let deleted = delete.await.unwrap().unwrap();
    let ingested = ingest.await.unwrap();
    match (deleted, ingested) {
        (DeleteProjectOutcome::Deleted, Err(AgentError::ProjectNotFound(missing_project))) => {
            assert_eq!(missing_project, project.id);
        }
        (DeleteProjectOutcome::NotEmpty, Ok(record)) => {
            assert_eq!(record.project_id, Some(project.id));
            store.delete_document(record.id).await.unwrap();
            assert_eq!(
                store.delete_project(project.id).await.unwrap(),
                DeleteProjectOutcome::Deleted
            );
        }
        (outcome, result) => {
            panic!("unexpected deletion/ingestion race result: {outcome:?}, {result:?}")
        }
    }
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
        raw_arguments: None,
        execution: ToolCallExecution::Client,
        status: ToolCallStatus::Pending,
        result: None,
        result_preview: None,
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
    // This slice exercises the terminal claim-scan receipt. Accepted turns now
    // use three attempts by default, so make the terminal boundary explicit just
    // as the equivalent SQLite coverage does.
    set_postgres_turn_max_attempts(&url, turn_id, 1).await;
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
    set_postgres_turn_max_attempts(&url, failure_turn.id, 1).await;
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
            None,
            &[],
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
            &[],
            Utc::now(),
        )
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(applied.outcome, ApplyTurnSteerOutcome::Applied(_)));
    assert_eq!(
        applied.event.event,
        AgentEvent::UserSteered {
            message_id: MessageId(steer_id.0),
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
            &[],
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
            &[],
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

#[tokio::test]
async fn postgres_user_questions_resume_exactly_and_serialize_with_cancellation() {
    let _guard = POSTGRES_TEST_LOCK.lock().await;
    let url = match std::env::var("OPENWAVE_POSTGRES_TEST_URL") {
        Ok(url) => url,
        Err(_) if std::env::var_os("OPENWAVE_REQUIRE_POSTGRES_TEST").is_some() => {
            panic!("OPENWAVE_POSTGRES_TEST_URL must name an isolated test database")
        }
        Err(_) => return,
    };
    let store = Arc::new(DbStore::connect(&url).await.unwrap());

    let park = |store: Arc<DbStore>, chat: Chat, turn_id: TurnId| async move {
        store.create_chat(&chat).await.unwrap();
        let accepted = match store
            .accept_turn(turn_id, chat.id, "gpt-5", "ask before acting")
            .await
            .unwrap()
        {
            AcceptTurnOutcome::Accepted(turn) => turn,
            outcome => panic!("unexpected question turn acceptance: {outcome:?}"),
        };
        let claimed_at = accepted.available_at + Duration::seconds(1);
        let (claimed, lease) = postgres_claim_exact_turn(&store, turn_id, claimed_at).await;
        assert_eq!(claimed.id, turn_id);
        let parked_at = claimed_at + Duration::seconds(1);
        let request = ClientToolCallRequest {
            id: CallId::new(),
            chat_id: chat.id,
            turn_id,
            provider_id: format!("question-{turn_id}"),
            name: ASK_USER_QUESTIONS_TOOL.into(),
            arguments: serde_json::json!({
                "questions": [{
                    "id": "target",
                    "header": "Target",
                    "question": "Where should I deploy?",
                    "options": [
                        {
                            "id": "staging",
                            "label": "Staging",
                            "description": "Deploy for verification."
                        },
                        {
                            "id": "production",
                            "label": "Production",
                            "description": "Deploy to customers."
                        }
                    ]
                }]
            }),
        };
        assert!(matches!(
            store
                .park_turn_for_client_tool_call(
                    turn_id,
                    lease,
                    0,
                    TurnCheckpointProgress {
                        model_steps: 1,
                        usage: Usage::default(),
                    },
                    parked_at,
                    &request,
                )
                .await
                .unwrap()
                .unwrap(),
            ParkTurnForClientCallOutcome::Parked {
                renderer_event: Some(_),
                ..
            }
        ));
        (request, parked_at)
    };

    let chat = sample_chat();
    let turn_id = TurnId::new();
    let (request, parked_at) = park(store.clone(), chat.clone(), turn_id).await;
    let answers = AnswerUserQuestions {
        answers: vec![UserQuestionAnswer {
            question_id: "target".into(),
            selected_option_ids: vec!["staging".into()],
            custom_answer: None,
        }],
        additional_user_context: None,
    };
    let answer_request = AnswerUserQuestionsRequest {
        chat_id: chat.id,
        call_id: request.id,
        answers: answers.clone(),
    };
    let answered_at = parked_at + Duration::seconds(1);
    let resume_scan_at = answered_at + Duration::seconds(1);
    let first_resume_lease = uuid::Uuid::new_v4();
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let answer_store = store.clone();
    let answer_barrier = barrier.clone();
    let submitted_answer = answer_request.clone();
    let answer_task = tokio::spawn(async move {
        answer_barrier.wait().await;
        answer_store
            .answer_user_questions(&submitted_answer, answered_at)
            .await
            .unwrap()
    });
    let claim_store = store.clone();
    let claim_task = tokio::spawn(async move {
        barrier.wait().await;
        claim_store
            .claim_turn_run(
                first_resume_lease,
                resume_scan_at,
                resume_scan_at + Duration::hours(1),
            )
            .await
            .unwrap()
    });
    assert!(matches!(
        answer_task.await.unwrap(),
        AnswerUserQuestionsOutcome::Answered { turn, .. }
            if turn.id == turn_id && turn.status == TurnRunStatus::Resuming
    ));
    let first_claim = claim_task.await.unwrap();
    let (resumed, resumed_lease) = if let Some(turn) = first_claim.turn {
        (turn, first_resume_lease)
    } else {
        postgres_claim_exact_turn(&store, turn_id, resume_scan_at + Duration::seconds(1)).await
    };
    assert_eq!(resumed.id, turn_id);
    assert_eq!((resumed.attempt_count, resumed.claim_count), (1, 2));
    assert!(matches!(
        store
            .answer_user_questions(&answer_request, answered_at)
            .await
            .unwrap(),
        AnswerUserQuestionsOutcome::Existing(turn) if turn.id == turn_id
    ));
    assert!(store
        .list_pending_user_questions(chat.id)
        .await
        .unwrap()
        .is_empty());
    let call = store
        .list_tool_calls(chat.id)
        .await
        .unwrap()
        .into_iter()
        .find(|call| call.id == request.id)
        .unwrap();
    assert_eq!(call.execution, ToolCallExecution::Orchestration);
    assert_eq!(call.status, ToolCallStatus::Completed);
    assert_eq!(
        serde_json::from_str::<AnswerUserQuestions>(call.result.as_deref().unwrap()).unwrap(),
        answers
    );
    let cancel_at = resume_scan_at + Duration::seconds(2);
    assert!(matches!(
        store
            .request_turn_cancellation(turn_id, cancel_at)
            .await
            .unwrap()
            .unwrap(),
        RequestTurnCancellationOutcome::Requested(_)
    ));
    assert!(matches!(
        store
            .finish_turn_cancellation(turn_id, resumed_lease, cancel_at + Duration::seconds(1),)
            .await
            .unwrap()
            .unwrap(),
        FinishTurnCancellationOutcome::Cancelled(_)
    ));

    let race_chat = sample_chat();
    let race_turn_id = TurnId::new();
    let (race_request, race_parked_at) = park(store.clone(), race_chat.clone(), race_turn_id).await;
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let answer_store = store.clone();
    let answer_barrier = barrier.clone();
    let race_answers = answers.clone();
    let answer_task = tokio::spawn(async move {
        answer_barrier.wait().await;
        answer_store
            .answer_user_questions(
                &AnswerUserQuestionsRequest {
                    chat_id: race_chat.id,
                    call_id: race_request.id,
                    answers: race_answers,
                },
                race_parked_at + Duration::seconds(1),
            )
            .await
            .unwrap()
    });
    let cancel_store = store.clone();
    let cancel_task = tokio::spawn(async move {
        barrier.wait().await;
        cancel_store
            .request_turn_cancellation(race_turn_id, race_parked_at + Duration::seconds(1))
            .await
            .unwrap()
            .unwrap()
    });
    assert!(matches!(
        answer_task.await.unwrap(),
        AnswerUserQuestionsOutcome::Answered { .. } | AnswerUserQuestionsOutcome::Unavailable
    ));
    assert!(matches!(
        cancel_task.await.unwrap(),
        RequestTurnCancellationOutcome::Cancelled(_) | RequestTurnCancellationOutcome::Existing(_)
    ));
    assert_eq!(
        store
            .get_turn_run(race_turn_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        TurnRunStatus::Cancelled
    );
    assert!(store
        .list_pending_user_questions(race_chat.id)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        store.delete_chat(chat.id).await.unwrap(),
        DeleteChatOutcome::Deleted
    );
    assert_eq!(
        store.delete_chat(race_chat.id).await.unwrap(),
        DeleteChatOutcome::Deleted
    );
}
