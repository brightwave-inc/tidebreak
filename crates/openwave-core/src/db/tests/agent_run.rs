use super::{sample_chat, temp_store};
use crate::{
    AcceptAgentRunOutcome, AcceptSandboxAgentRunAndParkTurnOutcome, AcceptTurnOutcome, AgentRun,
    AgentRunId, AgentRunInboxStatus, AgentRunStatus, AgentRunTier, CallId, ChatId,
    ClaimAgentRunInboxOutcome, ClaimSandboxToolCallOutcome,
    ConsumeAgentRunInboxAndResumeTurnOutcome, ConsumeAgentRunInboxOutcome, DbStore,
    FinishAgentRunCancellationOutcome, MessageId, ParkSandboxToolCallOutcome,
    ParkTurnForAgentRunInboxOutcome, RequestAgentRunCancellationOutcome, RequestFolderAccessArgs,
    RequestedFolderCapability, RequestedFolderHint, ResolveSandboxToolCallOutcome, Role,
    SandboxToolCallRequest, Store, SubmitAgentRunResultOutcome, ToolCallResolution,
    TurnCheckpointProgress, TurnId, TurnRunStatus, Usage,
};
use chrono::{Duration, Utc};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};

async fn accepted_sandbox_for_tool_test(store: &DbStore, chat_id: ChatId) -> AgentRun {
    admit_sandbox_for_test(store, chat_id, "Use the durable sandbox tool checkpoint").await
}

pub(super) async fn live_turn_for_sandbox_test(
    store: &DbStore,
    chat_id: ChatId,
) -> (crate::TurnRun, uuid::Uuid) {
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
            "sandbox-test-model",
            "sandbox test admission",
        )
        .await
        .unwrap();
    let lease = uuid::Uuid::new_v4();
    let now = Utc::now();
    let turn = store
        .claim_turn_run(lease, now, now + Duration::hours(1))
        .await
        .unwrap()
        .turn
        .expect("sandbox test turn should claim");
    assert_eq!(turn.id, turn_id);
    (turn, lease)
}

pub(super) async fn admit_sandbox_call_for_test(
    store: &DbStore,
    chat_id: ChatId,
    call_id: CallId,
    input: &str,
) -> AgentRun {
    let (turn, lease) = live_turn_for_sandbox_test(store, chat_id).await;
    match store
        .admit_sandbox_agent_run(
            turn.id,
            call_id,
            input,
            lease,
            turn.steer_revision,
            AgentRun::MAX_CONCURRENCY_LIMIT,
            Utc::now(),
        )
        .await
        .unwrap()
        .expect("sandbox test admission should resolve")
    {
        crate::AdmitSandboxAgentRunOutcome::Accepted { child, .. }
        | crate::AdmitSandboxAgentRunOutcome::Existing { child, .. } => child,
        outcome => panic!("unexpected sandbox test admission: {outcome:?}"),
    }
}

async fn admit_sandbox_for_test(store: &DbStore, chat_id: ChatId, input: &str) -> AgentRun {
    admit_sandbox_call_for_test(store, chat_id, CallId::new(), input).await
}

async fn force_expired_agent_lease(store: &DbStore, id: AgentRunId) {
    let past = Utc::now() - Duration::minutes(5);
    crate::db::entities::agent_run::Entity::update_many()
        .col_expr(
            crate::db::entities::agent_run::Column::LeaseExpiresAt,
            sea_orm::sea_query::Expr::value(Some(past)),
        )
        .filter(crate::db::entities::agent_run::Column::Id.eq(id.0))
        .exec(&store.conn)
        .await
        .unwrap();
}

async fn force_expired_agent_deadline(store: &DbStore, id: AgentRunId) {
    let deadline = Utc::now() - Duration::minutes(5);
    let created_at = deadline - Duration::hours(1);
    crate::db::entities::agent_run::Entity::update_many()
        .col_expr(
            crate::db::entities::agent_run::Column::CreatedAt,
            sea_orm::sea_query::Expr::value(created_at),
        )
        .col_expr(
            crate::db::entities::agent_run::Column::AvailableAt,
            sea_orm::sea_query::Expr::value(created_at),
        )
        .col_expr(
            crate::db::entities::agent_run::Column::DeadlineAt,
            sea_orm::sea_query::Expr::value(Some(deadline)),
        )
        .col_expr(
            crate::db::entities::agent_run::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(deadline),
        )
        .filter(crate::db::entities::agent_run::Column::Id.eq(id.0))
        .exec(&store.conn)
        .await
        .unwrap();
}

async fn submit_sandbox_result(
    store: &DbStore,
    chat_id: ChatId,
    input: &str,
    text: &str,
) -> (AgentRunId, crate::AgentRunResult) {
    let child_id = admit_sandbox_for_test(store, chat_id, input).await.id;
    let lease_token = uuid::Uuid::new_v4();
    let claimed = store
        .claim_agent_run(lease_token, Duration::minutes(1), 1, 1)
        .await
        .unwrap()
        .expect("sandbox child should claim");
    assert_eq!(claimed.id, child_id);
    let result = match store
        .submit_agent_run_result(child_id, lease_token, text)
        .await
        .unwrap()
        .expect("sandbox child should complete")
    {
        SubmitAgentRunResultOutcome::Completed(result) => result,
        outcome => panic!("unexpected result submission outcome: {outcome:?}"),
    };
    (child_id, result)
}

async fn park_foreground_turn_on_child(
    store: &DbStore,
    chat_id: ChatId,
    child_run_id: AgentRunId,
) -> crate::TurnRun {
    let (running, lease_token) = live_turn_for_sandbox_test(store, chat_id).await;
    let progress = TurnCheckpointProgress {
        model_steps: 1,
        usage: Usage {
            input_tokens: 1,
            output_tokens: 1,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
        },
    };
    match store
        .park_turn_for_agent_run_inbox(
            running.id,
            child_run_id,
            lease_token,
            running.steer_revision,
            progress,
            Utc::now(),
        )
        .await
        .unwrap()
        .expect("foreground turn should park")
    {
        ParkTurnForAgentRunInboxOutcome::Parked { turn, .. } => turn,
        outcome => panic!("unexpected child checkpoint outcome: {outcome:?}"),
    }
}

#[tokio::test]
async fn sandbox_spawn_and_foreground_checkpoint_commit_as_one_exact_transition() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let queued = match store
        .accept_turn(TurnId::new(), chat.id, "gpt-5", "delegate this atomically")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected turn acceptance: {outcome:?}"),
    };
    let lease_token = uuid::Uuid::new_v4();
    let now = Utc::now();
    let running = store
        .claim_turn_run(lease_token, now, now + Duration::minutes(5))
        .await
        .unwrap()
        .turn
        .expect("foreground turn should claim");
    assert_eq!(running.id, queued.id);
    let spawn_call_id = CallId::new();
    let child_run_id = AgentRunId::sandbox_for_spawn_call(spawn_call_id);
    let progress = TurnCheckpointProgress {
        model_steps: 1,
        usage: Usage {
            input_tokens: 11,
            output_tokens: 7,
            cache_read_input_tokens: 3,
            cache_creation_input_tokens: 2,
        },
    };
    let parked = match store
        .accept_sandbox_agent_run_and_park_turn(
            child_run_id,
            running.id,
            spawn_call_id,
            "research the question",
            lease_token,
            running.steer_revision,
            progress,
            Utc::now(),
        )
        .await
        .unwrap()
        .expect("live foreground turn should checkpoint")
    {
        AcceptSandboxAgentRunAndParkTurnOutcome::Parked { child, turn, wait } => {
            (child, turn, wait)
        }
        outcome => panic!("unexpected combined spawn outcome: {outcome:?}"),
    };
    assert_eq!(parked.0.id, child_run_id);
    assert_eq!(
        parked.0.parent_id,
        Some(AgentRunId::foreground_for_chat(chat.id))
    );
    assert_eq!(parked.0.status, AgentRunStatus::Queued);
    // The child inherits the origin turn's model in the same transaction, so its
    // executor never has to fall back to a boot default and the row records what
    // the run used.
    assert_eq!(parked.0.model.as_deref(), Some("gpt-5"));
    assert_eq!(parked.1.status, TurnRunStatus::WaitingForAgentRun);
    assert_eq!(parked.1.lease_token, None);
    assert_eq!(parked.2.turn_id, running.id);
    assert_eq!(
        parked.2.parent_run_id,
        AgentRunId::foreground_for_chat(chat.id)
    );
    assert_eq!(parked.2.progress, progress);

    assert!(matches!(
        store
            .accept_sandbox_agent_run_and_park_turn(
                AgentRunId::new(),
                running.id,
                spawn_call_id,
                "research the question",
                lease_token,
                running.steer_revision,
                progress,
                Utc::now(),
            )
            .await
            .unwrap(),
        Some(AcceptSandboxAgentRunAndParkTurnOutcome::IdentityConflict)
    ));
    let admission = store
        .get_sandbox_agent_admission(child_run_id)
        .await
        .unwrap()
        .expect("sandbox child should retain its exact origin ownership");
    assert_eq!(admission.child_run_id, child_run_id);
    assert_eq!(admission.origin_turn_id, running.id);
    assert_eq!(admission.spawn_call_id, spawn_call_id);
    assert!(matches!(
        store
            .accept_sandbox_agent_run_and_park_turn(
                child_run_id,
                running.id,
                spawn_call_id,
                "research the question",
                lease_token,
                running.steer_revision,
                progress,
                Utc::now(),
            )
            .await
            .unwrap(),
        Some(AcceptSandboxAgentRunAndParkTurnOutcome::Existing { child, turn, wait })
            if child == parked.0 && turn == parked.1 && wait == parked.2
    ));
    assert_eq!(store.list_agent_runs(chat.id).await.unwrap().len(), 2);
}

#[tokio::test]
async fn sandbox_spawn_checkpoint_rolls_back_when_foreground_lease_or_steer_is_stale() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let queued = match store
        .accept_turn(
            TurnId::new(),
            chat.id,
            "gpt-5",
            "do not enqueue a stale child",
        )
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected turn acceptance: {outcome:?}"),
    };
    let lease_token = uuid::Uuid::new_v4();
    let now = Utc::now();
    let running = store
        .claim_turn_run(lease_token, now, now + Duration::minutes(5))
        .await
        .unwrap()
        .turn
        .expect("foreground turn should claim");
    assert_eq!(running.id, queued.id);
    let progress = TurnCheckpointProgress {
        model_steps: 1,
        usage: Usage::default(),
    };
    let stale_call_id = CallId::new();
    let stale_child_id = AgentRunId::sandbox_for_spawn_call(stale_call_id);
    assert!(store
        .accept_sandbox_agent_run_and_park_turn(
            stale_child_id,
            running.id,
            stale_call_id,
            "this child must roll back",
            uuid::Uuid::new_v4(),
            running.steer_revision,
            progress,
            Utc::now(),
        )
        .await
        .unwrap()
        .is_none());
    assert!(store.get_agent_run(stale_child_id).await.unwrap().is_none());

    let superseded_call_id = CallId::new();
    let superseded_child_id = AgentRunId::sandbox_for_spawn_call(superseded_call_id);
    assert!(matches!(
        store
            .accept_sandbox_agent_run_and_park_turn(
                superseded_child_id,
                running.id,
                superseded_call_id,
                "this child must not survive a stale model output",
                lease_token,
                running.steer_revision + 1,
                progress,
                Utc::now(),
            )
            .await
            .unwrap(),
        Some(AcceptSandboxAgentRunAndParkTurnOutcome::OutputSuperseded(turn))
            if turn == running
    ));
    assert!(store
        .get_agent_run(superseded_child_id)
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        store.get_turn_run(running.id).await.unwrap(),
        Some(running),
        "a rolled-back spawn must preserve the exact live foreground claim"
    );
}

#[tokio::test]
async fn sandbox_spawn_checkpoint_never_retrofits_a_separately_accepted_child() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let queued = match store
        .accept_turn(
            TurnId::new(),
            chat.id,
            "gpt-5",
            "keep admission and waits coupled",
        )
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected turn acceptance: {outcome:?}"),
    };
    let lease_token = uuid::Uuid::new_v4();
    let now = Utc::now();
    let running = store
        .claim_turn_run(lease_token, now, now + Duration::minutes(5))
        .await
        .unwrap()
        .turn
        .expect("foreground turn should claim");
    assert_eq!(running.id, queued.id);
    let spawn_call_id = CallId::new();
    let child_run_id = AgentRunId::sandbox_for_spawn_call(spawn_call_id);
    store
        .admit_sandbox_agent_run(
            running.id,
            spawn_call_id,
            "accepted by an older boundary",
            lease_token,
            running.steer_revision,
            AgentRun::DEFAULT_MAX_OUTSTANDING_CHILDREN,
            Utc::now(),
        )
        .await
        .unwrap()
        .expect("standalone admission should resolve");
    let progress = TurnCheckpointProgress {
        model_steps: 1,
        usage: Usage::default(),
    };
    let legacy_parked = match store
        .park_turn_for_agent_run_inbox(
            running.id,
            child_run_id,
            lease_token,
            running.steer_revision,
            progress,
            Utc::now(),
        )
        .await
        .unwrap()
        .expect("legacy checkpoint should commit")
    {
        ParkTurnForAgentRunInboxOutcome::Parked { turn, wait } => (turn, wait),
        outcome => panic!("unexpected legacy checkpoint outcome: {outcome:?}"),
    };
    assert_eq!(legacy_parked.0.status, TurnRunStatus::WaitingForAgentRun);
    assert_eq!(legacy_parked.1.child_run_id, child_run_id);

    assert!(matches!(
        store
            .accept_sandbox_agent_run_and_park_turn(
                child_run_id,
                running.id,
                spawn_call_id,
                "accepted by an older boundary",
                lease_token,
                running.steer_revision,
                progress,
                Utc::now(),
            )
            .await
            .unwrap(),
        Some(AcceptSandboxAgentRunAndParkTurnOutcome::IdentityConflict)
    ));
    assert_eq!(
        store.get_turn_run(running.id).await.unwrap(),
        Some(legacy_parked.0)
    );
    assert_eq!(store.list_agent_runs(chat.id).await.unwrap().len(), 2);
}

#[tokio::test]
async fn sandbox_admission_is_exact_bounded_and_releases_consumed_terminal_capacity() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (turn, lease) = live_turn_for_sandbox_test(&store, chat.id).await;
    let first_call = CallId::new();
    let first = store
        .admit_sandbox_agent_run(
            turn.id,
            first_call,
            "first bounded child",
            lease,
            turn.steer_revision,
            1,
            Utc::now(),
        )
        .await
        .unwrap()
        .unwrap();
    let first_child = match first {
        crate::AdmitSandboxAgentRunOutcome::Accepted { child, admission } => {
            assert_eq!(admission.origin_turn_id, turn.id);
            assert_eq!(admission.parent_run_id, turn.agent_run_id);
            assert_eq!(admission.spawn_call_id, first_call);
            child
        }
        outcome => panic!("unexpected admission outcome: {outcome:?}"),
    };
    assert_eq!(
        first_child.id,
        AgentRunId::sandbox_for_spawn_call(first_call)
    );
    assert!(matches!(
        store
            .admit_sandbox_agent_run(
                turn.id,
                first_call,
                "first bounded child",
                lease,
                turn.steer_revision,
                1,
                Utc::now(),
            )
            .await
            .unwrap(),
        Some(crate::AdmitSandboxAgentRunOutcome::Existing { child, .. })
            if child == first_child
    ));

    let second_call = CallId::new();
    assert!(matches!(
        store
            .admit_sandbox_agent_run(
                turn.id,
                second_call,
                "second bounded child",
                lease,
                turn.steer_revision,
                1,
                Utc::now(),
            )
            .await
            .unwrap(),
        Some(crate::AdmitSandboxAgentRunOutcome::AtCapacity)
    ));

    let child_lease = uuid::Uuid::new_v4();
    assert_eq!(
        store
            .claim_agent_run(child_lease, Duration::minutes(1), 1, 1)
            .await
            .unwrap()
            .unwrap()
            .id,
        first_child.id
    );
    store
        .submit_agent_run_result(first_child.id, child_lease, "first child finished")
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        store
            .admit_sandbox_agent_run(
                turn.id,
                first_call,
                "first bounded child",
                lease,
                turn.steer_revision,
                1,
                Utc::now(),
            )
            .await
            .unwrap(),
        Some(crate::AdmitSandboxAgentRunOutcome::Existing { child, .. })
            if child.id == first_child.id
    ));
    assert!(store
        .get_sandbox_agent_admission(first_child.id)
        .await
        .unwrap()
        .is_some());
    assert!(matches!(
        store
            .admit_sandbox_agent_run(
                turn.id,
                second_call,
                "second bounded child",
                lease,
                turn.steer_revision,
                1,
                Utc::now(),
            )
            .await
            .unwrap(),
        Some(crate::AdmitSandboxAgentRunOutcome::AtCapacity)
    ));

    let parked = park_foreground_turn_on_child(&store, chat.id, first_child.id).await;
    assert_eq!(parked.id, turn.id);
    let continuation = uuid::Uuid::new_v4();
    store
        .claim_agent_run_inbox_entry(
            turn.agent_run_id,
            first_child.id,
            continuation,
            Duration::minutes(1),
        )
        .await
        .unwrap()
        .unwrap();
    store
        .consume_agent_run_inbox_entry_and_resume_turn(
            turn.agent_run_id,
            first_child.id,
            continuation,
        )
        .await
        .unwrap()
        .unwrap();
    let resumed_lease = uuid::Uuid::new_v4();
    let resumed = store
        .claim_turn_run(resumed_lease, Utc::now(), Utc::now() + Duration::minutes(5))
        .await
        .unwrap()
        .turn
        .unwrap();
    assert_eq!(resumed.id, turn.id);
    assert!(matches!(
        store
            .admit_sandbox_agent_run(
                resumed.id,
                second_call,
                "second bounded child",
                resumed_lease,
                resumed.steer_revision,
                1,
                Utc::now(),
            )
            .await
            .unwrap(),
        Some(crate::AdmitSandboxAgentRunOutcome::Accepted { child, .. })
            if child.id == AgentRunId::sandbox_for_spawn_call(second_call)
    ));
}

#[tokio::test]
async fn sandbox_admission_fails_closed_on_a_terminal_child_without_delivery() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (turn, lease) = live_turn_for_sandbox_test(&store, chat.id).await;
    let first_call = CallId::new();
    let first_child = match store
        .admit_sandbox_agent_run(
            turn.id,
            first_call,
            "first bounded child",
            lease,
            turn.steer_revision,
            1,
            Utc::now(),
        )
        .await
        .unwrap()
        .unwrap()
    {
        crate::AdmitSandboxAgentRunOutcome::Accepted { child, .. } => child,
        outcome => panic!("unexpected admission outcome: {outcome:?}"),
    };
    let child_lease = uuid::Uuid::new_v4();
    store
        .claim_agent_run(child_lease, Duration::minutes(1), 1, 1)
        .await
        .unwrap()
        .unwrap();
    store
        .submit_agent_run_result(first_child.id, child_lease, "first child finished")
        .await
        .unwrap()
        .unwrap();
    crate::db::entities::agent_run_inbox::Entity::delete_by_id(first_child.id.0)
        .exec(&store.conn)
        .await
        .unwrap();

    let second_call = CallId::new();
    let error = store
        .admit_sandbox_agent_run(
            turn.id,
            second_call,
            "second bounded child",
            lease,
            turn.steer_revision,
            1,
            Utc::now(),
        )
        .await
        .expect_err("missing terminal delivery must fail closed");
    assert!(matches!(error, crate::AgentError::Store(_)));
    assert!(store
        .get_agent_run(AgentRunId::sandbox_for_spawn_call(second_call))
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn concurrent_sandbox_admission_never_oversubscribes_origin_turn() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (turn, lease) = live_turn_for_sandbox_test(&store, chat.id).await;
    let store = std::sync::Arc::new(store);
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(8));
    let mut tasks = Vec::new();
    for index in 0..8 {
        let store = store.clone();
        let barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            store
                .admit_sandbox_agent_run(
                    turn.id,
                    CallId::new(),
                    &format!("concurrent child {index}"),
                    lease,
                    turn.steer_revision,
                    2,
                    Utc::now(),
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
        .filter(|outcome| matches!(outcome, crate::AdmitSandboxAgentRunOutcome::Accepted { .. }))
        .count();
    assert_eq!(accepted, 2);
    assert_eq!(
        store
            .list_agent_runs(chat.id)
            .await
            .unwrap()
            .into_iter()
            .filter(|run| run.tier == AgentRunTier::Background)
            .count(),
        2
    );
}

#[tokio::test]
async fn concurrent_exact_sandbox_admission_converges_on_one_receipt() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (turn, lease) = live_turn_for_sandbox_test(&store, chat.id).await;
    let call = CallId::new();
    let store = std::sync::Arc::new(store);
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    let mut tasks = Vec::new();
    for _ in 0..2 {
        let store = store.clone();
        let barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            store
                .admit_sandbox_agent_run(
                    turn.id,
                    call,
                    "the same concurrent child",
                    lease,
                    turn.steer_revision,
                    2,
                    Utc::now(),
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
                crate::AdmitSandboxAgentRunOutcome::Accepted { .. }
            )
        })
        .count();
    let existing = outcomes
        .iter()
        .filter(|outcome| {
            matches!(
                outcome.as_ref().unwrap(),
                crate::AdmitSandboxAgentRunOutcome::Existing { .. }
            )
        })
        .count();
    assert_eq!((accepted, existing), (1, 1));
    assert_eq!(
        store
            .list_agent_runs(chat.id)
            .await
            .unwrap()
            .into_iter()
            .filter(|run| run.tier == AgentRunTier::Background)
            .count(),
        1
    );
}

#[tokio::test]
async fn sandbox_child_cannot_park_a_different_origin_turn() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (first_turn, first_lease) = live_turn_for_sandbox_test(&store, chat.id).await;
    let child = match store
        .admit_sandbox_agent_run(
            first_turn.id,
            CallId::new(),
            "owned by only the first turn",
            first_lease,
            first_turn.steer_revision,
            2,
            Utc::now(),
        )
        .await
        .unwrap()
        .unwrap()
    {
        crate::AdmitSandboxAgentRunOutcome::Accepted { child, .. } => child,
        outcome => panic!("unexpected first admission: {outcome:?}"),
    };
    let cancelled_at = Utc::now();
    store
        .request_turn_cancellation(first_turn.id, cancelled_at)
        .await
        .unwrap();
    store
        .finish_turn_cancellation(first_turn.id, first_lease, Utc::now())
        .await
        .unwrap();

    let (second_turn, second_lease) = live_turn_for_sandbox_test(&store, chat.id).await;
    assert_ne!(second_turn.id, first_turn.id);
    assert!(matches!(
        store
            .park_turn_for_agent_run_inbox(
                second_turn.id,
                child.id,
                second_lease,
                second_turn.steer_revision,
                TurnCheckpointProgress {
                    model_steps: 1,
                    usage: Usage::default(),
                },
                Utc::now(),
            )
            .await
            .unwrap(),
        Some(ParkTurnForAgentRunInboxOutcome::IdentityConflict)
    ));
    assert_eq!(
        store.get_turn_run(second_turn.id).await.unwrap(),
        Some(second_turn)
    );
}

#[tokio::test]
async fn sandbox_admission_rejects_cross_turn_identity_and_fk_corruption() {
    let (_dir, store) = temp_store().await;
    let first_chat = sample_chat();
    let second_chat = sample_chat();
    store.create_chat(&first_chat).await.unwrap();
    store.create_chat(&second_chat).await.unwrap();
    let (first_turn, first_lease) = live_turn_for_sandbox_test(&store, first_chat.id).await;
    let (second_turn, second_lease) = live_turn_for_sandbox_test(&store, second_chat.id).await;
    let call = CallId::new();
    let child = match store
        .admit_sandbox_agent_run(
            first_turn.id,
            call,
            "owned by the first turn",
            first_lease,
            first_turn.steer_revision,
            2,
            Utc::now(),
        )
        .await
        .unwrap()
        .unwrap()
    {
        crate::AdmitSandboxAgentRunOutcome::Accepted { child, .. } => child,
        outcome => panic!("unexpected admission outcome: {outcome:?}"),
    };
    assert!(matches!(
        store
            .admit_sandbox_agent_run(
                second_turn.id,
                call,
                "owned by the first turn",
                second_lease,
                second_turn.steer_revision,
                2,
                Utc::now(),
            )
            .await
            .unwrap(),
        Some(crate::AdmitSandboxAgentRunOutcome::IdentityConflict)
    ));

    let malformed = crate::db::entities::sandbox_agent_admission::ActiveModel {
        child_run_id: Set(child.id.0),
        parent_run_id: Set(second_turn.agent_run_id.0),
        origin_turn_id: Set(second_turn.id.0),
        chat_id: Set(second_chat.id.0),
        spawn_call_id: Set(CallId::new().0),
        delegated_root_id: Set(None),
        delegated_relative_path: Set(None),
        admitted_at: Set(Utc::now()),
    };
    assert!(malformed.insert(&store.conn).await.is_err());
}

#[tokio::test]
async fn foreground_and_sandbox_runs_roundtrip_with_exact_idempotency() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();

    let foreground_id = AgentRunId::foreground_for_chat(chat.id);
    let foreground = store
        .get_agent_run(foreground_id)
        .await
        .unwrap()
        .expect("chat creation should create its foreground agent run");
    assert_eq!(foreground.id, foreground_id);
    assert_eq!(foreground.chat_id, chat.id);
    assert_eq!(foreground.parent_id, None);
    assert_eq!(foreground.depth, 0);
    assert_eq!(foreground.tier, AgentRunTier::Foreground);
    assert_eq!(foreground.status, AgentRunStatus::Active);
    assert_eq!(foreground.input, None);
    assert_eq!(foreground.attempt_count, 0);
    assert_eq!(foreground.max_attempts, 0);
    assert_eq!(foreground.claim_count, 0);
    assert_eq!(foreground.deadline_at, None);

    assert!(matches!(
        store
            .accept_agent_run(
                foreground_id,
                chat.id,
                None,
                None,
                AgentRunTier::Foreground,
                None,
            )
            .await
            .unwrap(),
        AcceptAgentRunOutcome::Existing(existing) if existing == foreground
    ));

    let spawn_call_id = CallId::new();
    let sandbox = admit_sandbox_call_for_test(
        &store,
        chat.id,
        spawn_call_id,
        "Inspect the selected documents",
    )
    .await;
    let sandbox_id = AgentRunId::sandbox_for_spawn_call(spawn_call_id);
    assert_eq!(sandbox.parent_id, Some(foreground_id));
    assert_eq!(sandbox.spawn_call_id, Some(spawn_call_id));
    assert_eq!(sandbox.depth, 1);
    assert_eq!(sandbox.tier, AgentRunTier::Background);
    assert_eq!(sandbox.status, AgentRunStatus::Queued);
    assert_eq!(sandbox.attempt_count, 0);
    assert_eq!(
        sandbox.max_attempts,
        crate::model::AgentRun::DEFAULT_MAX_ATTEMPTS
    );
    assert_eq!(sandbox.claim_count, 0);
    assert!(sandbox.deadline_at.is_some());
    assert_eq!(
        sandbox.input.as_deref(),
        Some("Inspect the selected documents")
    );

    assert!(matches!(
        admit_sandbox_call_for_test(
            &store,
            chat.id,
            spawn_call_id,
            "Inspect the selected documents",
        ).await,
        existing if existing == sandbox
    ));
    let (turn, lease) = live_turn_for_sandbox_test(&store, chat.id).await;
    assert!(matches!(
        store
            .admit_sandbox_agent_run(
                turn.id,
                spawn_call_id,
                "Changed delegated task",
                lease,
                turn.steer_revision,
                AgentRun::MAX_CONCURRENCY_LIMIT,
                Utc::now(),
            )
            .await
            .unwrap(),
        Some(crate::AdmitSandboxAgentRunOutcome::IdentityConflict)
    ));
    assert_eq!(
        store.get_agent_run(sandbox_id).await.unwrap(),
        Some(sandbox.clone())
    );
    let listed = store.list_agent_runs(chat.id).await.unwrap();
    assert_eq!(listed.len(), 2);
    assert!(listed.contains(&foreground));
    assert!(listed.contains(&sandbox));
}

#[tokio::test]
async fn agent_run_acceptance_enforces_one_foreground_and_depth_one_children() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let foreground_id = AgentRunId::foreground_for_chat(chat.id);
    assert!(matches!(
        store
            .accept_agent_run(
                AgentRunId::new(),
                chat.id,
                None,
                None,
                AgentRunTier::Foreground,
                None,
            )
            .await
            .unwrap(),
        AcceptAgentRunOutcome::ForegroundExists(existing) if existing.id == foreground_id
    ));

    let child_id = admit_sandbox_for_test(&store, chat.id, "first child")
        .await
        .id;
    assert_eq!(
        store.get_agent_run(child_id).await.unwrap().unwrap().depth,
        AgentRun::MAX_DEPTH
    );

    let other_chat = sample_chat();
    store.create_chat(&other_chat).await.unwrap();
    assert!(store
        .accept_agent_run(
            AgentRunId::new(),
            other_chat.id,
            Some(foreground_id),
            Some(CallId::new()),
            AgentRunTier::Background,
            Some("cross-chat child"),
        )
        .await
        .is_err());
}

#[tokio::test]
async fn agent_run_acceptance_rejects_invalid_shapes_and_identity_reuse() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let foreground_id = AgentRunId::foreground_for_chat(chat.id);

    assert!(store
        .accept_agent_run(
            AgentRunId::new(),
            chat.id,
            Some(foreground_id),
            None,
            AgentRunTier::Foreground,
            None,
        )
        .await
        .is_err());
    assert!(store
        .accept_agent_run(
            AgentRunId::new(),
            chat.id,
            Some(foreground_id),
            None,
            AgentRunTier::Background,
            Some("missing spawn identity"),
        )
        .await
        .is_err());
    assert!(store
        .accept_agent_run(
            AgentRunId::new(),
            chat.id,
            Some(foreground_id),
            Some(CallId::new()),
            AgentRunTier::Background,
            None,
        )
        .await
        .is_err());
    assert!(store
        .accept_agent_run(
            AgentRunId::new(),
            chat.id,
            Some(foreground_id),
            Some(CallId::new()),
            AgentRunTier::Background,
            Some(""),
        )
        .await
        .is_err());
    assert!(store
        .accept_agent_run(
            AgentRunId::from(uuid::Uuid::nil()),
            chat.id,
            Some(foreground_id),
            Some(CallId::new()),
            AgentRunTier::Background,
            Some("nil identity"),
        )
        .await
        .is_err());

    assert!(store
        .accept_agent_run(
            foreground_id,
            chat.id,
            Some(foreground_id),
            Some(CallId::new()),
            AgentRunTier::Background,
            Some("different immutable request"),
        )
        .await
        .is_err());
    assert!(store
        .list_agent_runs(ChatId::new())
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn agent_run_schema_rejects_cross_chat_parentage() {
    let (_dir, store) = temp_store().await;
    let parent_chat = sample_chat();
    let child_chat = sample_chat();
    store.create_chat(&parent_chat).await.unwrap();
    store.create_chat(&child_chat).await.unwrap();
    let parent_id = AgentRunId::foreground_for_chat(parent_chat.id);

    let now = chrono::Utc::now();
    let malformed = crate::db::entities::agent_run::ActiveModel {
        id: Set(AgentRunId::new().0),
        chat_id: Set(child_chat.id.0),
        parent_id: Set(Some(parent_id.0)),
        parent_depth: Set(Some(0)),
        spawn_call_id: Set(Some(CallId::new().0)),
        tier: Set(AgentRunTier::Background.as_str().into()),
        execution_location: Set(crate::model::AgentRunExecutionLocation::InProcess
            .as_str()
            .into()),
        depth: Set(1),
        status: Set(AgentRunStatus::Queued.as_str().into()),
        input: Set(Some("cross-chat raw insert".into())),
        model: Set(None),
        attempt_count: Set(0),
        max_attempts: Set(crate::model::AgentRun::DEFAULT_MAX_ATTEMPTS),
        claim_count: Set(0),
        available_at: Set(now),
        deadline_at: Set(Some(now + crate::model::AgentRun::DEFAULT_MAX_DURATION)),
        lease_token: Set(None),
        lease_expires_at: Set(None),
        started_at: Set(None),
        finished_at: Set(None),
        last_error_code: Set(None),
        last_error_detail: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    };
    assert!(malformed.insert(&store.conn).await.is_err());

    let partial_claim = crate::db::entities::agent_run_claim::ActiveModel {
        token: Set(uuid::Uuid::new_v4()),
        agent_run_id: Set(Some(AgentRunId::new().0)),
        attempt_count: Set(None),
        claim_count: Set(None),
        claimed_at: Set(now),
        lease_expires_at: Set(None),
    };
    assert!(partial_claim.insert(&store.conn).await.is_err());
}

#[tokio::test]
async fn scheduler_never_claims_a_sandbox_row_without_an_admission_receipt() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let parent_id = AgentRunId::foreground_for_chat(chat.id);
    let spawn_call_id = CallId::new();
    let child_id = AgentRunId::sandbox_for_spawn_call(spawn_call_id);
    let now = Utc::now();
    crate::db::entities::agent_run::ActiveModel {
        id: Set(child_id.0),
        chat_id: Set(chat.id.0),
        parent_id: Set(Some(parent_id.0)),
        parent_depth: Set(Some(0)),
        spawn_call_id: Set(Some(spawn_call_id.0)),
        tier: Set(AgentRunTier::Background.as_str().into()),
        execution_location: Set(crate::model::AgentRunExecutionLocation::InProcess
            .as_str()
            .into()),
        depth: Set(1),
        status: Set(AgentRunStatus::Queued.as_str().into()),
        input: Set(Some("receiptless corruption".into())),
        model: Set(None),
        attempt_count: Set(0),
        max_attempts: Set(AgentRun::DEFAULT_MAX_ATTEMPTS),
        claim_count: Set(0),
        available_at: Set(now),
        deadline_at: Set(Some(now + AgentRun::DEFAULT_MAX_DURATION)),
        lease_token: Set(None),
        lease_expires_at: Set(None),
        started_at: Set(None),
        finished_at: Set(None),
        last_error_code: Set(None),
        last_error_detail: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(&store.conn)
    .await
    .unwrap();

    assert!(store
        .claim_agent_run(uuid::Uuid::new_v4(), Duration::minutes(1), 4, 2)
        .await
        .unwrap()
        .is_none());
    let receiptless = store.get_agent_run(child_id).await.unwrap().unwrap();
    assert_eq!(receiptless.status, AgentRunStatus::Queued);
    assert_eq!(receiptless.claim_count, 0);

    let claim_token = uuid::Uuid::new_v4();
    let lease_expires_at = Utc::now() + Duration::minutes(5);
    crate::db::entities::agent_run_claim::ActiveModel {
        token: Set(claim_token),
        agent_run_id: Set(Some(child_id.0)),
        attempt_count: Set(Some(1)),
        claim_count: Set(Some(1)),
        claimed_at: Set(now),
        lease_expires_at: Set(Some(lease_expires_at)),
    }
    .insert(&store.conn)
    .await
    .unwrap();
    crate::db::entities::agent_run::Entity::update_many()
        .col_expr(
            crate::db::entities::agent_run::Column::Status,
            sea_orm::sea_query::Expr::value(AgentRunStatus::Running.as_str()),
        )
        .col_expr(
            crate::db::entities::agent_run::Column::AttemptCount,
            sea_orm::sea_query::Expr::value(1),
        )
        .col_expr(
            crate::db::entities::agent_run::Column::ClaimCount,
            sea_orm::sea_query::Expr::value(1),
        )
        .col_expr(
            crate::db::entities::agent_run::Column::LeaseToken,
            sea_orm::sea_query::Expr::value(Some(claim_token)),
        )
        .col_expr(
            crate::db::entities::agent_run::Column::LeaseExpiresAt,
            sea_orm::sea_query::Expr::value(Some(lease_expires_at)),
        )
        .col_expr(
            crate::db::entities::agent_run::Column::StartedAt,
            sea_orm::sea_query::Expr::value(Some(now)),
        )
        .col_expr(
            crate::db::entities::agent_run::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(crate::db::entities::agent_run::Column::Id.eq(child_id.0))
        .exec(&store.conn)
        .await
        .unwrap();
    assert!(store
        .claim_agent_run(claim_token, Duration::minutes(1), 4, 2)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn sandbox_claims_are_exact_reclaimable_and_heartbeatable() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let child_id = admit_sandbox_for_test(&store, chat.id, "claim and heartbeat")
        .await
        .id;

    let first_token = uuid::Uuid::new_v4();
    let first = store
        .claim_agent_run(first_token, Duration::minutes(1), 2, 1)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.id, child_id);
    assert_eq!(first.status, AgentRunStatus::Running);
    assert_eq!(first.attempt_count, 1);
    assert_eq!(first.claim_count, 1);
    assert_eq!(first.lease_token, Some(first_token));
    assert!(first.lease_expires_at.is_some());
    assert_eq!(
        store
            .claim_agent_run(first_token, Duration::minutes(1), 2, 1)
            .await
            .unwrap(),
        Some(first.clone())
    );

    assert!(!store
        .heartbeat_agent_run(child_id, uuid::Uuid::new_v4(), Duration::minutes(1))
        .await
        .unwrap());
    assert!(store
        .heartbeat_agent_run(child_id, first_token, Duration::minutes(2))
        .await
        .unwrap());
    assert!(!store
        .heartbeat_agent_run(child_id, first_token, Duration::minutes(1))
        .await
        .unwrap());
    assert!(!store
        .heartbeat_agent_run(
            child_id,
            first_token,
            AgentRun::DEFAULT_MAX_DURATION + Duration::seconds(1),
        )
        .await
        .unwrap());

    force_expired_agent_lease(&store, child_id).await;
    let second_token = uuid::Uuid::new_v4();
    let second = store
        .claim_agent_run(second_token, Duration::minutes(1), 2, 1)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(second.id, child_id);
    assert_eq!(second.attempt_count, 2);
    assert_eq!(second.claim_count, 2);
    assert_eq!(second.lease_token, Some(second_token));
    assert!(!store
        .heartbeat_agent_run(child_id, first_token, Duration::minutes(1))
        .await
        .unwrap());

    for expected_attempt in 3..=AgentRun::DEFAULT_MAX_ATTEMPTS {
        force_expired_agent_lease(&store, child_id).await;
        let next = store
            .claim_agent_run(uuid::Uuid::new_v4(), Duration::minutes(1), 2, 1)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(next.attempt_count, expected_attempt);
    }

    force_expired_agent_lease(&store, child_id).await;
    let exhausted_scan = uuid::Uuid::new_v4();
    assert!(store
        .claim_agent_run(exhausted_scan, Duration::minutes(1), 2, 1)
        .await
        .unwrap()
        .is_none());
    let failed = store.get_agent_run(child_id).await.unwrap().unwrap();
    assert_eq!(failed.status, AgentRunStatus::Failed);
    assert_eq!(failed.last_error_code.as_deref(), Some("lease_expired"));
    assert!(failed.finished_at.is_some());

    let later_chat = sample_chat();
    store.create_chat(&later_chat).await.unwrap();
    admit_sandbox_for_test(&store, later_chat.id, "later work").await;
    assert!(store
        .claim_agent_run(exhausted_scan, Duration::minutes(1), 2, 1)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn idle_polling_does_not_accrete_empty_claim_scan_rows() {
    let (_dir, store) = temp_store().await;

    // Stand in for a receipt an idle worker's earlier empty poll would have
    // left behind, well outside the retention window (#1455).
    let stale_token = uuid::Uuid::new_v4();
    let stale_claimed_at = Utc::now() - Duration::hours(2);
    crate::db::entities::agent_run_claim::ActiveModel {
        token: Set(stale_token),
        agent_run_id: Set(None),
        attempt_count: Set(None),
        claim_count: Set(None),
        claimed_at: Set(stale_claimed_at),
        lease_expires_at: Set(None),
    }
    .insert(&store.conn)
    .await
    .unwrap();

    // An idle worker poll with nothing to claim.
    assert!(store
        .claim_agent_run(uuid::Uuid::new_v4(), Duration::minutes(1), 4, 2)
        .await
        .unwrap()
        .is_none());

    let remaining = crate::db::entities::agent_run_claim::Entity::find()
        .filter(crate::db::entities::agent_run_claim::Column::AgentRunId.is_null())
        .all(&store.conn)
        .await
        .unwrap();
    // The stale receipt was swept; only the poll's own fresh receipt remains,
    // so an idle worker no longer accretes rows without bound.
    assert_eq!(remaining.len(), 1);
    assert_ne!(remaining[0].token, stale_token);
}

#[tokio::test]
async fn sandbox_result_submission_is_fenced_and_idempotent() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let child_id = admit_sandbox_for_test(&store, chat.id, "return a concise result")
        .await
        .id;
    let lease_token = uuid::Uuid::new_v4();
    store
        .claim_agent_run(lease_token, Duration::minutes(1), 1, 1)
        .await
        .unwrap()
        .unwrap();

    let result = match store
        .submit_agent_run_result(child_id, lease_token, "finished analysis")
        .await
        .unwrap()
        .unwrap()
    {
        SubmitAgentRunResultOutcome::Completed(result) => result,
        outcome => panic!("unexpected result submission outcome: {outcome:?}"),
    };
    assert_eq!(result.agent_run_id, child_id);
    assert_eq!(result.lease_token, lease_token);
    assert_eq!(result.text, "finished analysis");
    assert_eq!(
        store
            .list_agent_run_inbox(AgentRunId::foreground_for_chat(chat.id))
            .await
            .unwrap(),
        vec![crate::AgentRunInboxEntry {
            parent_run_id: AgentRunId::foreground_for_chat(chat.id),
            child_run_id: child_id,
            chat_id: chat.id,
            result: result.clone(),
            status: AgentRunInboxStatus::Pending,
            claim_count: 0,
            lease_token: None,
            lease_expires_at: None,
            consumed_lease_token: None,
            consumed_at: None,
            delivered_at: result.submitted_at,
        }]
    );
    let completed = store.get_agent_run(child_id).await.unwrap().unwrap();
    assert_eq!(completed.status, AgentRunStatus::Completed);
    assert_eq!(completed.lease_token, None);
    assert!(completed.finished_at.is_some());
    assert!(matches!(
        store
            .submit_agent_run_result(child_id, lease_token, "finished analysis")
            .await
            .unwrap(),
        Some(SubmitAgentRunResultOutcome::Existing(existing)) if existing == result
    ));
    assert!(store
        .submit_agent_run_result(child_id, uuid::Uuid::new_v4(), "finished analysis")
        .await
        .unwrap()
        .is_none());
    assert!(store
        .submit_agent_run_result(child_id, lease_token, "different result")
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn sandbox_folder_proposal_is_typed_fenced_and_idempotent() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let child_id = admit_sandbox_for_test(&store, chat.id, "ask the parent about folder consent")
        .await
        .id;
    let lease_token = uuid::Uuid::new_v4();
    store
        .claim_agent_run(lease_token, Duration::minutes(1), 1, 1)
        .await
        .unwrap()
        .unwrap();
    let request = RequestFolderAccessArgs {
        reason: "Read the documents needed for this task".into(),
        requested_capabilities: vec![RequestedFolderCapability::ReadFiles],
        folder_hint: Some(RequestedFolderHint::Documents),
    };
    let result = match store
        .submit_agent_run_folder_access_proposal(child_id, lease_token, &request)
        .await
        .unwrap()
        .unwrap()
    {
        SubmitAgentRunResultOutcome::Completed(result) => result,
        outcome => panic!("unexpected proposal submission outcome: {outcome:?}"),
    };
    assert!(matches!(
        &result.payload,
        crate::AgentRunResultPayload::FolderAccessProposal { request: stored } if stored == &request
    ));
    assert!(result.text.contains("This grants no access"));
    assert!(!result.text.contains("root_id"));
    assert!(matches!(
        store
            .submit_agent_run_folder_access_proposal(child_id, lease_token, &request)
            .await
            .unwrap(),
        Some(SubmitAgentRunResultOutcome::Existing(existing)) if existing == result
    ));
    let changed = RequestFolderAccessArgs {
        reason: "Read a different folder".into(),
        ..request.clone()
    };
    assert!(store
        .submit_agent_run_folder_access_proposal(child_id, lease_token, &changed)
        .await
        .unwrap()
        .is_none());
    assert!(store
        .submit_agent_run_folder_access_proposal(child_id, uuid::Uuid::new_v4(), &request)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn parent_inbox_consumption_is_fenced_and_idempotent() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let parent_id = AgentRunId::foreground_for_chat(chat.id);
    let (child_id, result) = submit_sandbox_result(
        &store,
        chat.id,
        "prepare a parent continuation",
        "completed child result",
    )
    .await;

    // A child can finish before the parent reaches the tool boundary. The
    // delivery stays pending until that exact foreground checkpoint exists.
    let early_lease = uuid::Uuid::new_v4();
    assert!(store
        .claim_agent_run_inbox_entry(parent_id, child_id, early_lease, Duration::minutes(1))
        .await
        .unwrap()
        .is_none());
    assert!(store
        .consume_agent_run_inbox_entry_and_resume_turn(parent_id, child_id, early_lease)
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        store.list_agent_run_inbox(parent_id).await.unwrap()[0].status,
        AgentRunInboxStatus::Pending
    );
    let parked = park_foreground_turn_on_child(&store, chat.id, child_id).await;
    assert_eq!(parked.status, TurnRunStatus::WaitingForAgentRun);

    let token = uuid::Uuid::new_v4();
    let claimed = match store
        .claim_agent_run_inbox_entry(parent_id, child_id, token, Duration::minutes(1))
        .await
        .unwrap()
        .expect("pending delivery should claim")
    {
        ClaimAgentRunInboxOutcome::Claimed(entry) => entry,
        outcome => panic!("unexpected inbox claim outcome: {outcome:?}"),
    };
    assert_eq!(claimed.result, result);
    assert_eq!(claimed.status, AgentRunInboxStatus::Claimed);
    assert_eq!(claimed.claim_count, 1);
    assert_eq!(claimed.lease_token, Some(token));
    assert!(claimed.lease_expires_at.is_some());
    assert_eq!(claimed.consumed_lease_token, None);
    assert_eq!(claimed.consumed_at, None);

    assert!(matches!(
        store
            .claim_agent_run_inbox_entry(parent_id, child_id, token, Duration::minutes(1))
            .await
            .unwrap(),
        Some(ClaimAgentRunInboxOutcome::Existing(entry)) if entry == claimed
    ));
    let stale_token = uuid::Uuid::new_v4();
    assert!(store
        .claim_agent_run_inbox_entry(parent_id, child_id, stale_token, Duration::minutes(1))
        .await
        .unwrap()
        .is_none());
    assert!(store
        .consume_agent_run_inbox_entry(parent_id, child_id, stale_token)
        .await
        .unwrap()
        .is_none());

    let consumed = match store
        .consume_agent_run_inbox_entry(parent_id, child_id, token)
        .await
        .unwrap()
        .expect("live lease should consume")
    {
        ConsumeAgentRunInboxOutcome::Consumed(entry) => entry,
        outcome => panic!("unexpected inbox consumption outcome: {outcome:?}"),
    };
    assert_eq!(consumed.status, AgentRunInboxStatus::Consumed);
    assert_eq!(consumed.claim_count, 1);
    assert_eq!(consumed.lease_token, None);
    assert_eq!(consumed.lease_expires_at, None);
    assert_eq!(consumed.consumed_lease_token, Some(token));
    assert!(consumed.consumed_at.is_some());
    assert!(matches!(
        store
            .consume_agent_run_inbox_entry(parent_id, child_id, token)
            .await
            .unwrap(),
        Some(ConsumeAgentRunInboxOutcome::Existing(entry)) if entry == consumed
    ));
    assert!(store
        .consume_agent_run_inbox_entry(parent_id, child_id, stale_token)
        .await
        .unwrap()
        .is_none());
    assert!(store
        .claim_agent_run_inbox_entry(
            parent_id,
            child_id,
            uuid::Uuid::new_v4(),
            Duration::minutes(1)
        )
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn child_inbox_consumption_atomically_wakes_a_parked_foreground_turn() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let parent_id = AgentRunId::foreground_for_chat(chat.id);
    let queued = match store
        .accept_turn(TurnId::new(), chat.id, "gpt-5", "delegate the research")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected turn acceptance: {outcome:?}"),
    };
    let turn_lease = uuid::Uuid::new_v4();
    let now = Utc::now();
    let running = store
        .claim_turn_run(turn_lease, now, now + Duration::minutes(5))
        .await
        .unwrap()
        .turn
        .expect("foreground turn should claim");
    assert_eq!(running.id, queued.id);
    let child_id = admit_sandbox_for_test(&store, chat.id, "research the question")
        .await
        .id;
    let progress = TurnCheckpointProgress {
        model_steps: 1,
        usage: Usage {
            input_tokens: 11,
            output_tokens: 7,
            cache_read_input_tokens: 3,
            cache_creation_input_tokens: 2,
        },
    };
    let parked = match store
        .park_turn_for_agent_run_inbox(
            running.id,
            child_id,
            turn_lease,
            running.steer_revision,
            progress,
            Utc::now(),
        )
        .await
        .unwrap()
        .expect("running turn should checkpoint")
    {
        ParkTurnForAgentRunInboxOutcome::Parked { turn, wait } => (turn, wait),
        outcome => panic!("unexpected child checkpoint outcome: {outcome:?}"),
    };
    assert_eq!(parked.0.status, TurnRunStatus::WaitingForAgentRun);
    assert_eq!(parked.1.child_run_id, child_id);
    assert_eq!(parked.1.parent_run_id, parent_id);
    assert_eq!(parked.1.progress, progress);

    let child_lease = uuid::Uuid::new_v4();
    let claimed_child = store
        .claim_agent_run(child_lease, Duration::minutes(1), 1, 1)
        .await
        .unwrap()
        .expect("sandbox child should claim");
    assert_eq!(claimed_child.id, child_id);
    store
        .submit_agent_run_result(child_id, child_lease, "research complete")
        .await
        .unwrap()
        .expect("sandbox result should deliver");
    let continuation_lease = uuid::Uuid::new_v4();
    store
        .claim_agent_run_inbox_entry(
            parent_id,
            child_id,
            continuation_lease,
            Duration::minutes(1),
        )
        .await
        .unwrap()
        .expect("delivered child result should claim");
    let resumed = match store
        .consume_agent_run_inbox_entry_and_resume_turn(parent_id, child_id, continuation_lease)
        .await
        .unwrap()
        .expect("claimed child result should wake its parent turn")
    {
        ConsumeAgentRunInboxAndResumeTurnOutcome::Resumed { inbox, turn } => (inbox, turn),
        outcome => panic!("unexpected inbox wake outcome: {outcome:?}"),
    };
    assert_eq!(resumed.0.status, AgentRunInboxStatus::Consumed);
    assert_eq!(resumed.0.consumed_lease_token, Some(continuation_lease));
    assert_eq!(resumed.1.status, TurnRunStatus::Resuming);
    assert_eq!(resumed.1.attempt_count, running.attempt_count);
    assert_eq!(resumed.1.claim_count, running.claim_count);
    assert_eq!(resumed.1.model_steps, progress.model_steps);
    assert_eq!(resumed.1.usage, progress.usage);
    let messages = store.list_messages(chat.id).await.unwrap();
    assert!(messages.iter().any(|message| {
        message.id == MessageId::sandbox_result_for_child(child_id)
            && message.turn_id == running.id
            && message.role == Role::System
            && message.content
                == "Sandbox agent completed. Its exact final result follows:\nresearch complete"
    }));
    assert!(matches!(
        store
            .consume_agent_run_inbox_entry_and_resume_turn(parent_id, child_id, continuation_lease)
            .await
            .unwrap(),
        Some(ConsumeAgentRunInboxAndResumeTurnOutcome::Existing { inbox, turn })
            if inbox == resumed.0 && turn == resumed.1
    ));
    assert!(store
        .consume_agent_run_inbox_entry_and_resume_turn(parent_id, child_id, uuid::Uuid::new_v4())
        .await
        .unwrap()
        .is_none());

    let resumed_lease = uuid::Uuid::new_v4();
    let resumed_claim = store
        .claim_turn_run(resumed_lease, Utc::now(), Utc::now() + Duration::minutes(5))
        .await
        .unwrap()
        .turn
        .expect("durable wake should make the turn claimable");
    assert_eq!(resumed_claim.id, running.id);
    assert_eq!(resumed_claim.status, TurnRunStatus::Running);
    assert_eq!(resumed_claim.attempt_count, running.attempt_count);
    assert_eq!(resumed_claim.claim_count, running.claim_count + 1);
    assert_eq!(resumed_claim.model_steps, progress.model_steps);
    assert_eq!(resumed_claim.usage, progress.usage);
}

#[tokio::test]
async fn parked_parent_cancellation_retires_a_delivered_child_without_an_orphan_wake() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();

    let parent_id = AgentRunId::foreground_for_chat(chat.id);
    let child_id = admit_sandbox_for_test(&store, chat.id, "finish before the parent is cancelled")
        .await
        .id;
    let parked = park_foreground_turn_on_child(&store, chat.id, child_id).await;
    let child_lease = uuid::Uuid::new_v4();
    assert_eq!(
        store
            .claim_agent_run(child_lease, Duration::minutes(1), 4, 2)
            .await
            .unwrap()
            .unwrap()
            .id,
        child_id
    );
    store
        .submit_agent_run_result(child_id, child_lease, "terminal child result")
        .await
        .unwrap()
        .expect("child result should be delivered before cancellation");

    assert!(matches!(
        store.request_turn_cancellation(parked.id, Utc::now()).await.unwrap(),
        Some(crate::RequestTurnCancellationOutcome::Cancelled(turn))
            if turn.status == TurnRunStatus::Cancelled
    ));
    let inbox = store.list_agent_run_inbox(parent_id).await.unwrap();
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].status, AgentRunInboxStatus::Cancelled);
    assert!(store
        .list_agent_run_inbox_candidates(16)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        store
            .list_agent_runs(chat.id)
            .await
            .unwrap()
            .into_iter()
            .find(|run| run.id == child_id)
            .unwrap()
            .status,
        AgentRunStatus::Completed,
        "an already-terminal child remains auditable, but its delivery is fenced"
    );
}

#[tokio::test]
async fn parked_parent_cancellation_cascades_to_an_unclaimed_sandbox_child() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();

    let child_id = admit_sandbox_for_test(
        &store,
        chat.id,
        "this work should be fenced before it starts",
    )
    .await
    .id;
    let parked = park_foreground_turn_on_child(&store, chat.id, child_id).await;
    assert!(matches!(
        store
            .request_turn_cancellation(parked.id, Utc::now())
            .await
            .unwrap(),
        Some(crate::RequestTurnCancellationOutcome::Cancelled(_))
    ));
    let child = store
        .list_agent_runs(chat.id)
        .await
        .unwrap()
        .into_iter()
        .find(|run| run.id == child_id)
        .unwrap();
    assert_eq!(child.status, AgentRunStatus::Cancelled);
    assert!(store
        .claim_agent_run(uuid::Uuid::new_v4(), Duration::minutes(1), 4, 2)
        .await
        .unwrap()
        .is_none());
    let inbox = store
        .list_agent_run_inbox(AgentRunId::foreground_for_chat(chat.id))
        .await
        .unwrap();
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].status, AgentRunInboxStatus::Cancelled);
    assert!(matches!(
        inbox[0].result.payload,
        crate::AgentRunResultPayload::Cancelled {
            reason: crate::AgentRunCancellationReason::ParentTurnCancelled
        }
    ));
}

#[tokio::test]
async fn failed_child_result_is_persisted_in_the_resumed_parent_transcript() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let parent_id = AgentRunId::foreground_for_chat(chat.id);
    let child_id = admit_sandbox_for_test(&store, chat.id, "this task will fail")
        .await
        .id;
    crate::db::entities::agent_run::Entity::update_many()
        .col_expr(
            crate::db::entities::agent_run::Column::MaxAttempts,
            sea_orm::sea_query::Expr::value(1),
        )
        .filter(crate::db::entities::agent_run::Column::Id.eq(child_id.0))
        .exec(&store.conn)
        .await
        .unwrap();
    let parked = park_foreground_turn_on_child(&store, chat.id, child_id).await;
    let child_lease = uuid::Uuid::new_v4();
    store
        .claim_agent_run(child_lease, Duration::minutes(1), 4, 2)
        .await
        .unwrap()
        .expect("child should claim");
    assert!(matches!(
        store
            .fail_agent_run(
                child_id,
                child_lease,
                "sandbox_execution_failed",
                "provider failed",
                Duration::seconds(1),
            )
            .await
            .unwrap(),
        Some(crate::FailAgentRunOutcome::Failed(_))
    ));
    let continuation_lease = uuid::Uuid::new_v4();
    store
        .claim_agent_run_inbox_entry(
            parent_id,
            child_id,
            continuation_lease,
            Duration::minutes(1),
        )
        .await
        .unwrap()
        .expect("failed result should claim for the parked parent");
    store
        .consume_agent_run_inbox_entry_and_resume_turn(parent_id, child_id, continuation_lease)
        .await
        .unwrap()
        .expect("failed result should resume the parked parent");
    assert!(store.list_messages(chat.id).await.unwrap().iter().any(|message| {
        message.id == MessageId::sandbox_result_for_child(child_id)
            && message.turn_id == parked.id
            && message.role == Role::System
            && message.content
                == "Sandbox agent failed. Its exact final result follows:\nSandbox task failed (sandbox_execution_failed): provider failed"
    }));
}

#[tokio::test]
async fn live_parent_inbox_candidates_skip_sixteen_obsolete_deliveries() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();

    for index in 0..16 {
        submit_sandbox_result(
            &store,
            chat.id,
            &format!("obsolete task {index}"),
            &format!("obsolete result {index}"),
        )
        .await;
    }
    let (live_child, _) = submit_sandbox_result(
        &store,
        chat.id,
        "the live delegated task",
        "the live result",
    )
    .await;
    let parked = park_foreground_turn_on_child(&store, chat.id, live_child).await;
    assert_eq!(parked.status, TurnRunStatus::WaitingForAgentRun);

    let candidates = store.list_agent_run_inbox_candidates(16).await.unwrap();
    assert_eq!(
        candidates
            .iter()
            .map(|entry| entry.child_run_id)
            .collect::<Vec<_>>(),
        vec![live_child],
        "obsolete entries must not starve the bounded recovery scan"
    );
}

#[tokio::test]
async fn expired_parent_inbox_lease_can_be_reclaimed_but_not_consumed_by_stale_owner() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let parent_id = AgentRunId::foreground_for_chat(chat.id);
    let (child_id, _) = submit_sandbox_result(
        &store,
        chat.id,
        "exercise recovery",
        "recoverable child result",
    )
    .await;
    park_foreground_turn_on_child(&store, chat.id, child_id).await;
    let first_token = uuid::Uuid::new_v4();
    store
        .claim_agent_run_inbox_entry(parent_id, child_id, first_token, Duration::minutes(1))
        .await
        .unwrap()
        .expect("pending delivery should claim");
    let expired_at = Utc::now() - Duration::minutes(5);
    crate::db::entities::agent_run_inbox::Entity::update_many()
        .col_expr(
            crate::db::entities::agent_run_inbox::Column::LeaseExpiresAt,
            sea_orm::sea_query::Expr::value(Some(expired_at)),
        )
        .filter(crate::db::entities::agent_run_inbox::Column::ChildRunId.eq(child_id.0))
        .exec(&store.conn)
        .await
        .unwrap();

    assert!(store
        .claim_agent_run_inbox_entry(parent_id, child_id, first_token, Duration::minutes(1))
        .await
        .unwrap()
        .is_none());
    assert!(store
        .consume_agent_run_inbox_entry(parent_id, child_id, first_token)
        .await
        .unwrap()
        .is_none());

    let replacement_token = uuid::Uuid::new_v4();
    let reclaimed = match store
        .claim_agent_run_inbox_entry(parent_id, child_id, replacement_token, Duration::minutes(1))
        .await
        .unwrap()
        .expect("expired delivery should reclaim")
    {
        ClaimAgentRunInboxOutcome::Claimed(entry) => entry,
        outcome => panic!("unexpected inbox claim outcome: {outcome:?}"),
    };
    assert_eq!(reclaimed.claim_count, 2);
    assert_eq!(reclaimed.lease_token, Some(replacement_token));
    assert!(store
        .consume_agent_run_inbox_entry(parent_id, child_id, first_token)
        .await
        .unwrap()
        .is_none());
    assert!(matches!(
        store
            .consume_agent_run_inbox_entry(parent_id, child_id, replacement_token)
            .await
            .unwrap(),
        Some(ConsumeAgentRunInboxOutcome::Consumed(entry))
            if entry.claim_count == 2
                && entry.consumed_lease_token == Some(replacement_token)
    ));
}

#[tokio::test]
async fn sandbox_cancellation_fences_running_work_and_recovers_exact_acknowledgement() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let queued_id = admit_sandbox_for_test(&store, chat.id, "cancel before claim")
        .await
        .id;
    assert!(matches!(
        store.request_agent_run_cancellation(queued_id).await.unwrap(),
        Some(RequestAgentRunCancellationOutcome::Cancelled(run)) if run.status == AgentRunStatus::Cancelled
    ));
    assert!(matches!(
        store.request_agent_run_cancellation(queued_id).await.unwrap(),
        Some(RequestAgentRunCancellationOutcome::Existing(run)) if run.status == AgentRunStatus::Cancelled
    ));
    assert!(store
        .claim_agent_run(uuid::Uuid::new_v4(), Duration::minutes(1), 1, 1)
        .await
        .unwrap()
        .is_none());

    let running_id = admit_sandbox_for_test(&store, chat.id, "cancel while running")
        .await
        .id;
    let lease_token = uuid::Uuid::new_v4();
    store
        .claim_agent_run(lease_token, Duration::minutes(1), 1, 1)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        store
            .request_agent_run_cancellation(running_id)
            .await
            .unwrap(),
        Some(RequestAgentRunCancellationOutcome::Requested(run)) if run.status == AgentRunStatus::Cancelling
    ));
    assert!(store
        .submit_agent_run_result(running_id, lease_token, "too late")
        .await
        .unwrap()
        .is_none());
    assert!(store
        .finish_agent_run_cancellation(running_id, uuid::Uuid::new_v4())
        .await
        .unwrap()
        .is_none());
    let cancelled = match store
        .finish_agent_run_cancellation(running_id, lease_token)
        .await
        .unwrap()
        .unwrap()
    {
        FinishAgentRunCancellationOutcome::Cancelled(run) => run,
        outcome => panic!("unexpected cancellation outcome: {outcome:?}"),
    };
    assert_eq!(cancelled.status, AgentRunStatus::Cancelled);
    assert!(matches!(
        store
            .finish_agent_run_cancellation(running_id, lease_token)
            .await
            .unwrap(),
        Some(FinishAgentRunCancellationOutcome::Existing(existing)) if existing == cancelled
    ));
}

#[tokio::test]
async fn operational_claim_count_cannot_use_cancellation_provenance_sentinel() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let child = admit_sandbox_for_test(&store, chat.id, "reserve cancellation provenance").await;
    assert!(crate::db::entities::agent_run::Entity::update_many()
        .col_expr(
            crate::db::entities::agent_run::Column::ClaimCount,
            sea_orm::sea_query::Expr::value(i32::MAX),
        )
        .filter(crate::db::entities::agent_run::Column::Id.eq(child.id.0))
        .exec(&store.conn)
        .await
        .is_err());
}

#[tokio::test]
async fn direct_cancellation_reason_wins_a_later_parent_cancellation() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (origin, origin_lease) = live_turn_for_sandbox_test(&store, chat.id).await;
    let child = admit_sandbox_for_test(&store, chat.id, "preserve first cancellation cause").await;
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
            .request_agent_run_cancellation(child.id)
            .await
            .unwrap(),
        Some(RequestAgentRunCancellationOutcome::Requested(_))
    ));
    assert!(matches!(
        store
            .request_turn_cancellation(origin.id, Utc::now())
            .await
            .unwrap(),
        Some(crate::RequestTurnCancellationOutcome::Requested(turn))
            if turn.status == TurnRunStatus::Cancelling
    ));
    let child_after_parent = store.get_agent_run(child.id).await.unwrap().unwrap();
    assert_eq!(child_after_parent.status, AgentRunStatus::Cancelling);
    assert_eq!(child_after_parent.lease_token, Some(child_lease));

    assert!(matches!(
        store
            .finish_agent_run_cancellation(child.id, child_lease)
            .await
            .unwrap(),
        Some(FinishAgentRunCancellationOutcome::Cancelled(_))
    ));
    let inbox = store
        .list_agent_run_inbox(AgentRunId::foreground_for_chat(chat.id))
        .await
        .unwrap();
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].status, AgentRunInboxStatus::Cancelled);
    assert!(matches!(
        inbox[0].result.payload,
        crate::AgentRunResultPayload::Cancelled {
            reason: crate::AgentRunCancellationReason::Requested
        }
    ));
    assert!(matches!(
        store
            .finish_turn_cancellation(origin.id, origin_lease, Utc::now())
            .await
            .unwrap(),
        Some(crate::FinishTurnCancellationOutcome::Cancelled(_))
    ));
}

#[tokio::test]
async fn cancellation_retry_remains_pending_while_parent_completion_is_fenced() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (origin, origin_lease) = live_turn_for_sandbox_test(&store, chat.id).await;
    let child = admit_sandbox_for_test(&store, chat.id, "cancel before parent completes").await;
    store
        .request_agent_run_cancellation(child.id)
        .await
        .unwrap()
        .unwrap();
    let completed_at = Utc::now().max(origin.updated_at);
    let output = crate::Message {
        id: MessageId::new(),
        chat_id: chat.id,
        turn_id: origin.id,
        role: Role::Assistant,
        content: "parent completed independently".into(),
        created_at: completed_at,
    };
    assert!(matches!(
        store
            .complete_turn_run(origin.id, origin_lease, 0, completed_at, &output)
            .await
            .unwrap(),
        Some(crate::CompleteTurnRunOutcome::ChildrenOutstanding { child_run_ids, .. })
            if child_run_ids == vec![child.id]
    ));
    assert!(matches!(
        store.request_agent_run_cancellation(child.id).await.unwrap(),
        Some(RequestAgentRunCancellationOutcome::Existing(run))
            if run.status == AgentRunStatus::Cancelled
    ));
    let inbox = store
        .list_agent_run_inbox(AgentRunId::foreground_for_chat(chat.id))
        .await
        .unwrap();
    assert_eq!(inbox[0].status, AgentRunInboxStatus::Pending);
}

#[tokio::test]
async fn parent_cancellation_preserves_a_live_child_lease_until_exact_ack() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (origin, origin_lease) = live_turn_for_sandbox_test(&store, chat.id).await;
    let child = admit_sandbox_for_test(&store, chat.id, "parent will cancel this live task").await;
    let child_lease = uuid::Uuid::new_v4();
    store
        .claim_agent_run(child_lease, Duration::minutes(5), 1, 1)
        .await
        .unwrap()
        .unwrap();

    store
        .request_turn_cancellation(origin.id, Utc::now())
        .await
        .unwrap()
        .unwrap();
    let cancelling = store.get_agent_run(child.id).await.unwrap().unwrap();
    assert_eq!(cancelling.status, AgentRunStatus::Cancelling);
    assert_eq!(cancelling.lease_token, Some(child_lease));
    assert!(store
        .list_agent_run_inbox(AgentRunId::foreground_for_chat(chat.id))
        .await
        .unwrap()
        .is_empty());
    assert!(store
        .submit_agent_run_result(child.id, child_lease, "late success")
        .await
        .unwrap()
        .is_none());

    store
        .finish_agent_run_cancellation(child.id, child_lease)
        .await
        .unwrap()
        .unwrap();
    let inbox = store
        .list_agent_run_inbox(AgentRunId::foreground_for_chat(chat.id))
        .await
        .unwrap();
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].status, AgentRunInboxStatus::Cancelled);
    assert!(matches!(
        inbox[0].result.payload,
        crate::AgentRunResultPayload::Cancelled {
            reason: crate::AgentRunCancellationReason::ParentTurnCancelled
        }
    ));
    store
        .finish_turn_cancellation(origin.id, origin_lease, Utc::now())
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn single_wait_resumes_with_deterministic_cancellation_context() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let parent_id = AgentRunId::foreground_for_chat(chat.id);
    let child = admit_sandbox_for_test(&store, chat.id, "cancel this parked task").await;
    let parked = park_foreground_turn_on_child(&store, chat.id, child.id).await;
    store
        .request_agent_run_cancellation(child.id)
        .await
        .unwrap()
        .unwrap();
    let continuation = uuid::Uuid::new_v4();
    store
        .claim_agent_run_inbox_entry(parent_id, child.id, continuation, Duration::minutes(1))
        .await
        .unwrap()
        .unwrap();
    let resumed = store
        .consume_agent_run_inbox_entry_and_resume_turn(parent_id, child.id, continuation)
        .await
        .unwrap()
        .unwrap();
    let ConsumeAgentRunInboxAndResumeTurnOutcome::Resumed { inbox, turn } = resumed else {
        panic!("unexpected exact cancellation resume: {resumed:?}")
    };
    assert_eq!(turn.id, parked.id);
    assert_eq!(turn.status, TurnRunStatus::Resuming);
    assert!(matches!(
        inbox.result.payload,
        crate::AgentRunResultPayload::Cancelled {
            reason: crate::AgentRunCancellationReason::Requested
        }
    ));
    let messages = store.list_messages(chat.id).await.unwrap();
    assert!(messages.iter().any(|message| {
        message.role == Role::System && message.content.ends_with("Sandbox task was cancelled.")
    }));
}

#[tokio::test]
async fn expired_sandbox_cancellation_is_terminal_and_cannot_fake_worker_acknowledgement() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let expired_before_request =
        admit_sandbox_for_test(&store, chat.id, "cancel after lease expiry")
            .await
            .id;
    store
        .claim_agent_run(uuid::Uuid::new_v4(), Duration::minutes(1), 1, 1)
        .await
        .unwrap()
        .unwrap();
    force_expired_agent_lease(&store, expired_before_request).await;
    assert!(matches!(
        store
            .request_agent_run_cancellation(expired_before_request)
            .await
            .unwrap(),
        Some(RequestAgentRunCancellationOutcome::Cancelled(run))
            if run.status == AgentRunStatus::Cancelled && run.lease_token.is_none()
    ));
    assert!(store
        .claim_agent_run(uuid::Uuid::new_v4(), Duration::minutes(1), 1, 1)
        .await
        .unwrap()
        .is_none());

    let expired_after_request =
        admit_sandbox_for_test(&store, chat.id, "expire after cancellation request")
            .await
            .id;
    let lease_token = uuid::Uuid::new_v4();
    store
        .claim_agent_run(lease_token, Duration::minutes(1), 1, 1)
        .await
        .unwrap()
        .unwrap();
    store
        .request_agent_run_cancellation(expired_after_request)
        .await
        .unwrap();
    force_expired_agent_lease(&store, expired_after_request).await;
    assert!(store
        .claim_agent_run(uuid::Uuid::new_v4(), Duration::minutes(1), 1, 1)
        .await
        .unwrap()
        .is_none());
    assert!(matches!(
        store
            .finish_agent_run_cancellation(expired_after_request, lease_token)
            .await
            .unwrap(),
        Some(FinishAgentRunCancellationOutcome::Existing(run))
            if run.status == AgentRunStatus::Cancelled
    ));
}

#[tokio::test]
async fn unavailable_parent_rejects_result_without_holding_the_scheduler_lock() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    let other_chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    store.create_chat(&other_chat).await.unwrap();
    let child_id = admit_sandbox_for_test(&store, chat.id, "parent may finish first")
        .await
        .id;
    let lease_token = uuid::Uuid::new_v4();
    store
        .claim_agent_run(lease_token, Duration::minutes(1), 2, 1)
        .await
        .unwrap()
        .unwrap();
    let other_child_id = admit_sandbox_for_test(&store, other_chat.id, "scheduler remains usable")
        .await
        .id;
    let now = Utc::now();
    crate::db::entities::agent_run::Entity::update_many()
        .col_expr(
            crate::db::entities::agent_run::Column::Status,
            sea_orm::sea_query::Expr::value(AgentRunStatus::Completed.as_str()),
        )
        .col_expr(
            crate::db::entities::agent_run::Column::FinishedAt,
            sea_orm::sea_query::Expr::value(Some(now)),
        )
        .col_expr(
            crate::db::entities::agent_run::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(
            crate::db::entities::agent_run::Column::Id
                .eq(AgentRunId::foreground_for_chat(chat.id).0),
        )
        .exec(&store.conn)
        .await
        .unwrap();
    assert!(store
        .submit_agent_run_result(child_id, lease_token, "orphaned result")
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        store
            .claim_agent_run(uuid::Uuid::new_v4(), Duration::minutes(1), 2, 1)
            .await
            .unwrap()
            .unwrap()
            .id,
        other_child_id
    );
}

#[tokio::test]
async fn sandbox_claims_enforce_global_and_per_chat_limits() {
    let (_dir, store) = temp_store().await;
    let first_chat = sample_chat();
    let second_chat = sample_chat();
    store.create_chat(&first_chat).await.unwrap();
    store.create_chat(&second_chat).await.unwrap();
    let mut accepted = Vec::new();
    for (chat_id, task) in [
        (first_chat.id, "first-a"),
        (first_chat.id, "first-b"),
        (second_chat.id, "second-a"),
    ] {
        let run = admit_sandbox_for_test(&store, chat_id, task).await;
        accepted.push(run);
    }

    let first = store
        .claim_agent_run(uuid::Uuid::new_v4(), Duration::minutes(1), 2, 1)
        .await
        .unwrap()
        .unwrap();
    let second = store
        .claim_agent_run(uuid::Uuid::new_v4(), Duration::minutes(1), 2, 1)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(first.chat_id, second.chat_id);
    assert!(store
        .claim_agent_run(uuid::Uuid::new_v4(), Duration::minutes(1), 2, 1)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn sandbox_claim_terminalizes_expired_deadlines() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let child = admit_sandbox_for_test(&store, chat.id, "deadline").await;
    let child_id = child.id;
    force_expired_agent_deadline(&store, child.id).await;
    assert!(store
        .claim_agent_run(uuid::Uuid::new_v4(), Duration::minutes(1), 1, 1)
        .await
        .unwrap()
        .is_none());
    let failed = store.get_agent_run(child_id).await.unwrap().unwrap();
    assert_eq!(failed.status, AgentRunStatus::Failed);
    assert_eq!(failed.last_error_code.as_deref(), Some("deadline_exceeded"));
    assert_eq!(
        store
            .list_agent_run_inbox(AgentRunId::foreground_for_chat(chat.id))
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn expired_cancelling_sandbox_run_is_reaped_and_releases_capacity() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    let other_chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    store.create_chat(&other_chat).await.unwrap();
    let child_id = admit_sandbox_for_test(&store, chat.id, "cancellation reaper")
        .await
        .id;
    store
        .claim_agent_run(uuid::Uuid::new_v4(), Duration::minutes(1), 2, 1)
        .await
        .unwrap()
        .unwrap();
    let other_child_id =
        admit_sandbox_for_test(&store, other_chat.id, "capacity-consuming live worker")
            .await
            .id;
    let other = store
        .claim_agent_run(uuid::Uuid::new_v4(), Duration::minutes(1), 2, 1)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(other.id, other_child_id);
    crate::db::entities::agent_run::Entity::update_many()
        .col_expr(
            crate::db::entities::agent_run::Column::Status,
            sea_orm::sea_query::Expr::value(AgentRunStatus::Cancelling.as_str()),
        )
        .filter(crate::db::entities::agent_run::Column::Id.eq(child_id.0))
        .exec(&store.conn)
        .await
        .unwrap();
    force_expired_agent_lease(&store, child_id).await;

    assert!(store
        .claim_agent_run(uuid::Uuid::new_v4(), Duration::minutes(1), 1, 1)
        .await
        .unwrap()
        .is_none());
    let cancelled = store.get_agent_run(child_id).await.unwrap().unwrap();
    assert_eq!(cancelled.status, AgentRunStatus::Cancelled);
    assert!(cancelled.finished_at.is_some());
    assert_eq!(cancelled.lease_token, None);
}

#[tokio::test]
async fn concurrent_sandbox_claimers_respect_both_limits() {
    let (_dir, store) = temp_store().await;
    let first_chat = sample_chat();
    let second_chat = sample_chat();
    store.create_chat(&first_chat).await.unwrap();
    store.create_chat(&second_chat).await.unwrap();
    for chat_id in [first_chat.id, first_chat.id, second_chat.id, second_chat.id] {
        let _accepted = admit_sandbox_for_test(&store, chat_id, "concurrent claim").await;
    }

    let store = std::sync::Arc::new(store);
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(8));
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let store = store.clone();
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
async fn sandbox_tool_checkpoint_is_lease_fenced_and_receipt_idempotent() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let sandbox = accepted_sandbox_for_tool_test(&store, chat.id).await;
    let worker_lease = uuid::Uuid::new_v4();
    assert_eq!(
        store
            .claim_agent_run(worker_lease, Duration::minutes(5), 1, 1)
            .await
            .unwrap()
            .unwrap()
            .id,
        sandbox.id
    );
    let request = SandboxToolCallRequest {
        id: CallId::new(),
        agent_run_id: sandbox.id,
        chat_id: chat.id,
        provider_id: "provider-call-1".into(),
        name: "web_search".into(),
        arguments: serde_json::json!({"query": "durable receipts"}),
    };
    assert!(matches!(
        store
            .park_agent_run_for_sandbox_tool_call(sandbox.id, worker_lease, &request)
            .await
            .unwrap(),
        ParkSandboxToolCallOutcome::Parked { .. }
    ));
    assert!(matches!(
        store
            .park_agent_run_for_sandbox_tool_call(sandbox.id, worker_lease, &request)
            .await
            .unwrap(),
        ParkSandboxToolCallOutcome::Existing { .. }
    ));
    let executor_lease = uuid::Uuid::new_v4();
    assert!(matches!(
        store
            .claim_sandbox_tool_call(request.id, executor_lease, Duration::minutes(2))
            .await
            .unwrap(),
        ClaimSandboxToolCallOutcome::Claimed(_)
    ));
    assert!(matches!(
        store
            .claim_sandbox_tool_call(request.id, executor_lease, Duration::minutes(2))
            .await
            .unwrap(),
        ClaimSandboxToolCallOutcome::Existing(_)
    ));
    let resolution = ToolCallResolution::Completed {
        result: "search results".into(),
    };
    assert_eq!(
        store
            .resolve_sandbox_tool_call(request.id, executor_lease, &resolution)
            .await
            .unwrap(),
        ResolveSandboxToolCallOutcome::Resolved
    );
    assert_eq!(
        store
            .resolve_sandbox_tool_call(request.id, executor_lease, &resolution)
            .await
            .unwrap(),
        ResolveSandboxToolCallOutcome::Existing
    );
    assert_eq!(
        store
            .resolve_sandbox_tool_call(
                request.id,
                executor_lease,
                &ToolCallResolution::Cancelled {
                    result: "different".into(),
                },
            )
            .await
            .unwrap(),
        ResolveSandboxToolCallOutcome::AlreadyTerminal
    );
    assert_eq!(
        store
            .get_agent_run(sandbox.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        AgentRunStatus::RetryWait
    );
    let resumed = store
        .claim_agent_run(uuid::Uuid::new_v4(), Duration::minutes(2), 1, 1)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resumed.id, sandbox.id);
    assert_eq!(resumed.attempt_count, 1, "tool continuation is not a retry");
    assert_eq!(resumed.claim_count, 2);
    assert_eq!(
        store
            .get_sandbox_tool_call_receipt(request.id)
            .await
            .unwrap()
            .unwrap()
            .result,
        "search results"
    );
}

#[tokio::test]
async fn cancelling_waiting_sandbox_fences_claimed_tool_work() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let sandbox = accepted_sandbox_for_tool_test(&store, chat.id).await;
    let worker_lease = uuid::Uuid::new_v4();
    store
        .claim_agent_run(worker_lease, Duration::minutes(5), 1, 1)
        .await
        .unwrap();
    let request = SandboxToolCallRequest {
        id: CallId::new(),
        agent_run_id: sandbox.id,
        chat_id: chat.id,
        provider_id: "provider-call-2".into(),
        name: "web_search".into(),
        arguments: serde_json::json!({"query": "cancel"}),
    };
    store
        .park_agent_run_for_sandbox_tool_call(sandbox.id, worker_lease, &request)
        .await
        .unwrap();
    let executor_lease = uuid::Uuid::new_v4();
    store
        .claim_sandbox_tool_call(request.id, executor_lease, Duration::minutes(2))
        .await
        .unwrap();
    assert!(matches!(
        store
            .request_agent_run_cancellation(sandbox.id)
            .await
            .unwrap(),
        Some(RequestAgentRunCancellationOutcome::Cancelled(_))
    ));
    assert_eq!(
        store
            .resolve_sandbox_tool_call(
                request.id,
                executor_lease,
                &ToolCallResolution::Completed {
                    result: "late result".into(),
                },
            )
            .await
            .unwrap(),
        ResolveSandboxToolCallOutcome::AlreadyTerminal
    );
    assert_eq!(
        store
            .get_sandbox_tool_call_receipt(request.id)
            .await
            .unwrap()
            .unwrap()
            .status
            .as_str(),
        "cancelled"
    );
}

#[tokio::test]
async fn expired_sandbox_tool_claim_is_recoverable_and_fences_stale_executor() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let sandbox = accepted_sandbox_for_tool_test(&store, chat.id).await;
    let worker_lease = uuid::Uuid::new_v4();
    store
        .claim_agent_run(worker_lease, Duration::minutes(5), 1, 1)
        .await
        .unwrap();
    let request = SandboxToolCallRequest {
        id: CallId::new(),
        agent_run_id: sandbox.id,
        chat_id: chat.id,
        provider_id: "provider-call-3".into(),
        name: "web_search".into(),
        arguments: serde_json::json!({"query": "expiry"}),
    };
    store
        .park_agent_run_for_sandbox_tool_call(sandbox.id, worker_lease, &request)
        .await
        .unwrap();
    let stale_executor = uuid::Uuid::new_v4();
    store
        .claim_sandbox_tool_call(request.id, stale_executor, Duration::minutes(2))
        .await
        .unwrap();
    crate::db::entities::sandbox_tool_call::Entity::update_many()
        .col_expr(
            crate::db::entities::sandbox_tool_call::Column::ExecutorLeaseExpiresAt,
            sea_orm::sea_query::Expr::value(Some(Utc::now() - Duration::minutes(1))),
        )
        .filter(crate::db::entities::sandbox_tool_call::Column::Id.eq(request.id.0))
        .exec(&store.conn)
        .await
        .unwrap();
    assert_eq!(
        store
            .list_sandbox_tool_call_candidates(8)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(matches!(
        store
            .claim_sandbox_tool_call(request.id, uuid::Uuid::new_v4(), Duration::minutes(2))
            .await
            .unwrap(),
        ClaimSandboxToolCallOutcome::Unavailable
    ));
    assert_eq!(
        store
            .resolve_sandbox_tool_call(
                request.id,
                stale_executor,
                &ToolCallResolution::Completed {
                    result: "late result".into(),
                },
            )
            .await
            .unwrap(),
        ResolveSandboxToolCallOutcome::AlreadyTerminal
    );
    let receipt = store
        .get_sandbox_tool_call_receipt(request.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        receipt.error_code.as_deref(),
        Some("executor_lease_expired")
    );
    assert_eq!(
        store
            .get_agent_run(sandbox.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        AgentRunStatus::RetryWait
    );
}

#[tokio::test]
async fn waiting_sandbox_deadline_fences_tool_and_delivers_parent_failure() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let sandbox = accepted_sandbox_for_tool_test(&store, chat.id).await;
    let worker_lease = uuid::Uuid::new_v4();
    store
        .claim_agent_run(worker_lease, Duration::minutes(5), 1, 1)
        .await
        .unwrap();
    let request = SandboxToolCallRequest {
        id: CallId::new(),
        agent_run_id: sandbox.id,
        chat_id: chat.id,
        provider_id: "provider-call-4".into(),
        name: "web_search".into(),
        arguments: serde_json::json!({"query": "deadline"}),
    };
    store
        .park_agent_run_for_sandbox_tool_call(sandbox.id, worker_lease, &request)
        .await
        .unwrap();
    force_expired_agent_deadline(&store, sandbox.id).await;
    assert!(store
        .claim_agent_run(uuid::Uuid::new_v4(), Duration::minutes(1), 1, 1)
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        store
            .get_agent_run(sandbox.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        AgentRunStatus::Failed
    );
    assert_eq!(
        store
            .get_sandbox_tool_call_receipt(request.id)
            .await
            .unwrap()
            .unwrap()
            .error_code
            .as_deref(),
        Some("deadline_exceeded")
    );
    assert_eq!(
        store
            .list_agent_run_inbox(AgentRunId::foreground_for_chat(chat.id))
            .await
            .unwrap()
            .len(),
        1
    );
}

/// The tier/location split is a persisted-value contract: every pre-split row
/// must come back readable, with `sandbox` mapped to `(background, in_process)`
/// and `foreground` to `(foreground, in_process)`.
#[tokio::test]
async fn m0021_maps_existing_execution_rows_onto_tier_and_location() {
    use sea_orm::ConnectionTrait;
    use sea_orm_migration::MigratorTrait;

    let dir = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("execution-split-upgrade.db").display()
    );
    let conn = sea_orm::Database::connect(&url).await.unwrap();
    let split_index = crate::db::migration::Migrator::migrations()
        .iter()
        .position(|migration| migration.name() == "m20260728_000021_split_agent_run_execution")
        .expect("the split migration is registered");
    crate::db::migration::Migrator::up(&conn, Some(split_index as u32))
        .await
        .unwrap();

    let chat_id = ChatId::new();
    let foreground_id = AgentRunId::foreground_for_chat(chat_id);
    let spawn_call_id = CallId::new();
    let child_id = AgentRunId::sandbox_for_spawn_call(spawn_call_id);
    // Seed pre-split rows through the old single execution column. The chat
    // row is deliberately absent: this exercises exactly the agent_run
    // mapping, and enforcement stays off on this connection for the batch.
    conn.execute_unprepared(&format!(
        "PRAGMA foreign_keys=OFF;\n\
         INSERT INTO agent_run \
         (id, chat_id, parent_id, parent_depth, spawn_call_id, execution, depth, status, input, \
          attempt_count, max_attempts, claim_count, available_at, deadline_at, created_at, updated_at) \
         VALUES (X'{foreground}', X'{chat}', NULL, NULL, NULL, 'foreground', 0, 'active', NULL, \
          0, 0, 0, '2026-07-01 00:00:00+00:00', NULL, \
          '2026-07-01 00:00:00+00:00', '2026-07-01 00:00:00+00:00');\n\
         INSERT INTO agent_run \
         (id, chat_id, parent_id, parent_depth, spawn_call_id, execution, depth, status, input, \
          attempt_count, max_attempts, claim_count, available_at, deadline_at, created_at, updated_at) \
         VALUES (X'{child}', X'{chat}', X'{foreground}', 0, X'{spawn}', 'sandbox', 1, 'queued', \
          'legacy delegated task', 0, 3, 0, '2026-07-01 00:00:01+00:00', \
          '2026-07-01 00:30:00+00:00', '2026-07-01 00:00:01+00:00', '2026-07-01 00:00:01+00:00');",
        foreground = foreground_id.0.simple(),
        chat = chat_id.0.simple(),
        child = child_id.0.simple(),
        spawn = spawn_call_id.0.simple(),
    ))
    .await
    .unwrap();

    crate::db::migration::Migrator::up(&conn, None)
        .await
        .unwrap();

    let store = DbStore { conn };
    let foreground = store
        .get_agent_run(foreground_id)
        .await
        .unwrap()
        .expect("the pre-split foreground run survives");
    assert_eq!(foreground.tier, AgentRunTier::Foreground);
    assert_eq!(
        foreground.execution_location,
        crate::AgentRunExecutionLocation::InProcess
    );
    let child = store
        .get_agent_run(child_id)
        .await
        .unwrap()
        .expect("the pre-split sandbox run survives");
    assert_eq!(child.tier, AgentRunTier::Background);
    assert_eq!(
        child.execution_location,
        crate::AgentRunExecutionLocation::InProcess
    );
    assert_eq!(child.parent_id, Some(foreground_id));
    assert_eq!(child.input.as_deref(), Some("legacy delegated task"));
}

#[tokio::test]
async fn active_work_counts_gate_host_quiescence() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();

    // The always-present foreground coordinator run never blocks quiescence:
    // a freshly created chat reports no active work.
    assert!(store.count_active_work().await.unwrap().is_quiescent());

    let turn_id = TurnId::new();
    store
        .accept_turn(turn_id, chat.id, "gpt-5", "hello")
        .await
        .unwrap();

    let spawn_call_id = CallId::new();
    let child_id = AgentRunId::sandbox_for_spawn_call(spawn_call_id);
    let now = Utc::now();
    crate::db::entities::agent_run::ActiveModel {
        id: Set(child_id.0),
        chat_id: Set(chat.id.0),
        parent_id: Set(Some(AgentRunId::foreground_for_chat(chat.id).0)),
        parent_depth: Set(Some(0)),
        spawn_call_id: Set(Some(spawn_call_id.0)),
        tier: Set(AgentRunTier::Background.as_str().into()),
        execution_location: Set(crate::model::AgentRunExecutionLocation::InProcess
            .as_str()
            .into()),
        depth: Set(1),
        status: Set(AgentRunStatus::Queued.as_str().into()),
        input: Set(Some("delegated background task".into())),
        model: Set(None),
        attempt_count: Set(0),
        max_attempts: Set(AgentRun::DEFAULT_MAX_ATTEMPTS),
        claim_count: Set(0),
        available_at: Set(now),
        deadline_at: Set(Some(now + AgentRun::DEFAULT_MAX_DURATION)),
        lease_token: Set(None),
        lease_expires_at: Set(None),
        started_at: Set(None),
        finished_at: Set(None),
        last_error_code: Set(None),
        last_error_detail: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(&store.conn)
    .await
    .unwrap();

    let busy = store.count_active_work().await.unwrap();
    assert_eq!(busy.active_turns, 1);
    assert_eq!(busy.live_background_runs, 1);
    assert!(!busy.is_quiescent());

    // Settled rows drop out of both counts.
    let mut settled_turn: crate::db::entities::turn_run::ActiveModel =
        crate::db::entities::turn_run::Entity::find_by_id(turn_id.0)
            .one(&store.conn)
            .await
            .unwrap()
            .unwrap()
            .into();
    settled_turn.status = Set(TurnRunStatus::Cancelled.as_str().into());
    settled_turn.finished_at = Set(Some(now));
    settled_turn.update(&store.conn).await.unwrap();

    let mut settled_run: crate::db::entities::agent_run::ActiveModel =
        crate::db::entities::agent_run::Entity::find_by_id((child_id.0, chat.id.0, 1))
            .one(&store.conn)
            .await
            .unwrap()
            .unwrap()
            .into();
    settled_run.status = Set(AgentRunStatus::Cancelled.as_str().into());
    settled_run.finished_at = Set(Some(now));
    settled_run.update(&store.conn).await.unwrap();

    assert!(store.count_active_work().await.unwrap().is_quiescent());
}
