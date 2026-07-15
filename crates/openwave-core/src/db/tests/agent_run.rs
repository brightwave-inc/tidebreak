use super::{sample_chat, temp_store};
use crate::{
    AcceptAgentRunOutcome, AcceptSandboxAgentRunAndParkTurnOutcome, AcceptTurnOutcome, AgentRun,
    AgentRunExecution, AgentRunId, AgentRunInboxStatus, AgentRunStatus, CallId, ChatId,
    ClaimAgentRunInboxOutcome, ConsumeAgentRunInboxAndResumeTurnOutcome,
    ConsumeAgentRunInboxOutcome, DbStore, FinishAgentRunCancellationOutcome, MessageId,
    ParkTurnForAgentRunInboxOutcome, RequestAgentRunCancellationOutcome, Role, Store,
    SubmitAgentRunResultOutcome, TurnCheckpointProgress, TurnId, TurnRunStatus, Usage,
};
use chrono::{Duration, Utc};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};

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
    let child_id = AgentRunId::new();
    store
        .accept_agent_run(
            child_id,
            chat_id,
            Some(AgentRunId::foreground_for_chat(chat_id)),
            Some(CallId::new()),
            AgentRunExecution::Sandbox,
            Some(input),
        )
        .await
        .unwrap();
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
    let queued = match store
        .accept_turn(TurnId::new(), chat_id, "gpt-5", "wait for the child")
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
    let child_run_id = AgentRunId::new();
    let spawn_call_id = CallId::new();
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
    let stale_child_id = AgentRunId::new();
    assert!(store
        .accept_sandbox_agent_run_and_park_turn(
            stale_child_id,
            running.id,
            CallId::new(),
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

    let superseded_child_id = AgentRunId::new();
    assert!(matches!(
        store
            .accept_sandbox_agent_run_and_park_turn(
                superseded_child_id,
                running.id,
                CallId::new(),
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
    let child_run_id = AgentRunId::new();
    let spawn_call_id = CallId::new();
    store
        .accept_agent_run(
            child_run_id,
            chat.id,
            Some(AgentRunId::foreground_for_chat(chat.id)),
            Some(spawn_call_id),
            AgentRunExecution::Sandbox,
            Some("accepted by an older boundary"),
        )
        .await
        .unwrap();
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
                AgentRunId::new(),
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
    assert_eq!(foreground.execution, AgentRunExecution::Foreground);
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
                AgentRunExecution::Foreground,
                None,
            )
            .await
            .unwrap(),
        AcceptAgentRunOutcome::Existing(existing) if existing == foreground
    ));

    let sandbox_id = AgentRunId::new();
    let spawn_call_id = CallId::new();
    let sandbox = match store
        .accept_agent_run(
            sandbox_id,
            chat.id,
            Some(foreground_id),
            Some(spawn_call_id),
            AgentRunExecution::Sandbox,
            Some("Inspect the selected documents"),
        )
        .await
        .unwrap()
    {
        AcceptAgentRunOutcome::Accepted(run) => run,
        outcome => panic!("unexpected sandbox outcome: {outcome:?}"),
    };
    assert_eq!(sandbox.parent_id, Some(foreground_id));
    assert_eq!(sandbox.spawn_call_id, Some(spawn_call_id));
    assert_eq!(sandbox.depth, 1);
    assert_eq!(sandbox.execution, AgentRunExecution::Sandbox);
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
        store
            .accept_agent_run(
                sandbox_id,
                chat.id,
                Some(foreground_id),
                Some(spawn_call_id),
                AgentRunExecution::Sandbox,
                Some("Inspect the selected documents"),
            )
            .await
            .unwrap(),
        AcceptAgentRunOutcome::Existing(existing) if existing == sandbox
    ));
    assert!(matches!(
        store
            .accept_agent_run(
                AgentRunId::new(),
                chat.id,
                Some(foreground_id),
                Some(spawn_call_id),
                AgentRunExecution::Sandbox,
                Some("Inspect the selected documents"),
            )
            .await
            .unwrap(),
        AcceptAgentRunOutcome::Existing(existing) if existing == sandbox
    ));
    assert!(matches!(
        store
            .accept_agent_run(
                AgentRunId::new(),
                chat.id,
                Some(foreground_id),
                Some(spawn_call_id),
                AgentRunExecution::Sandbox,
                Some("Changed delegated task"),
            )
            .await
            .unwrap(),
        AcceptAgentRunOutcome::IdentityConflict
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
                AgentRunExecution::Foreground,
                None,
            )
            .await
            .unwrap(),
        AcceptAgentRunOutcome::ForegroundExists(existing) if existing.id == foreground_id
    ));

    let child_id = AgentRunId::new();
    assert!(matches!(
        store
            .accept_agent_run(
                child_id,
                chat.id,
                Some(foreground_id),
                Some(CallId::new()),
                AgentRunExecution::Sandbox,
                Some("first child"),
            )
            .await
            .unwrap(),
        AcceptAgentRunOutcome::Accepted(_)
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

    let other_chat = sample_chat();
    store.create_chat(&other_chat).await.unwrap();
    assert!(matches!(
        store
            .accept_agent_run(
                AgentRunId::new(),
                other_chat.id,
                Some(foreground_id),
                Some(CallId::new()),
                AgentRunExecution::Sandbox,
                Some("cross-chat child"),
            )
            .await
            .unwrap(),
        AcceptAgentRunOutcome::ParentUnavailable
    ));
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
            AgentRunExecution::Foreground,
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
            AgentRunExecution::Sandbox,
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
            AgentRunExecution::Sandbox,
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
            AgentRunExecution::Sandbox,
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
            AgentRunExecution::Sandbox,
            Some("nil identity"),
        )
        .await
        .is_err());

    assert!(matches!(
        store
            .accept_agent_run(
                foreground_id,
                chat.id,
                Some(foreground_id),
                Some(CallId::new()),
                AgentRunExecution::Sandbox,
                Some("different immutable request"),
            )
            .await
            .unwrap(),
        AcceptAgentRunOutcome::IdentityConflict
    ));
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
        execution: Set(AgentRunExecution::Sandbox.as_str().into()),
        depth: Set(1),
        status: Set(AgentRunStatus::Queued.as_str().into()),
        input: Set(Some("cross-chat raw insert".into())),
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
async fn sandbox_claims_are_exact_reclaimable_and_heartbeatable() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let foreground_id = AgentRunId::foreground_for_chat(chat.id);
    let child_id = AgentRunId::new();
    let _child = match store
        .accept_agent_run(
            child_id,
            chat.id,
            Some(foreground_id),
            Some(CallId::new()),
            AgentRunExecution::Sandbox,
            Some("claim and heartbeat"),
        )
        .await
        .unwrap()
    {
        AcceptAgentRunOutcome::Accepted(run) => run,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };

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
        .heartbeat_agent_run(child_id, first_token, Duration::minutes(2))
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

    force_expired_agent_lease(&store, child_id).await;
    let third_token = uuid::Uuid::new_v4();
    let third = store
        .claim_agent_run(third_token, Duration::minutes(1), 2, 1)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(third.attempt_count, 3);

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
    store
        .accept_agent_run(
            AgentRunId::new(),
            later_chat.id,
            Some(AgentRunId::foreground_for_chat(later_chat.id)),
            Some(CallId::new()),
            AgentRunExecution::Sandbox,
            Some("later work"),
        )
        .await
        .unwrap();
    assert!(store
        .claim_agent_run(exhausted_scan, Duration::minutes(1), 2, 1)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn sandbox_result_submission_is_fenced_and_idempotent() {
    let (_dir, store) = temp_store().await;
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
            Some("return a concise result"),
        )
        .await
        .unwrap();
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
    let child_id = AgentRunId::new();
    store
        .accept_agent_run(
            child_id,
            chat.id,
            Some(parent_id),
            Some(CallId::new()),
            AgentRunExecution::Sandbox,
            Some("research the question"),
        )
        .await
        .unwrap();
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

    let child_id = AgentRunId::new();
    let parent_id = AgentRunId::foreground_for_chat(chat.id);
    store
        .accept_agent_run(
            child_id,
            chat.id,
            Some(parent_id),
            Some(CallId::new()),
            AgentRunExecution::Sandbox,
            Some("finish before the parent is cancelled"),
        )
        .await
        .unwrap();
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

    let child_id = AgentRunId::new();
    store
        .accept_agent_run(
            child_id,
            chat.id,
            Some(AgentRunId::foreground_for_chat(chat.id)),
            Some(CallId::new()),
            AgentRunExecution::Sandbox,
            Some("this work should be fenced before it starts"),
        )
        .await
        .unwrap();
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
    assert!(store
        .list_agent_run_inbox(AgentRunId::foreground_for_chat(chat.id))
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn failed_child_result_is_persisted_in_the_resumed_parent_transcript() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let child_id = AgentRunId::new();
    let parent_id = AgentRunId::foreground_for_chat(chat.id);
    store
        .accept_agent_run(
            child_id,
            chat.id,
            Some(parent_id),
            Some(CallId::new()),
            AgentRunExecution::Sandbox,
            Some("this task will fail"),
        )
        .await
        .unwrap();
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
    let queued_id = AgentRunId::new();
    store
        .accept_agent_run(
            queued_id,
            chat.id,
            Some(AgentRunId::foreground_for_chat(chat.id)),
            Some(CallId::new()),
            AgentRunExecution::Sandbox,
            Some("cancel before claim"),
        )
        .await
        .unwrap();
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

    let running_id = AgentRunId::new();
    store
        .accept_agent_run(
            running_id,
            chat.id,
            Some(AgentRunId::foreground_for_chat(chat.id)),
            Some(CallId::new()),
            AgentRunExecution::Sandbox,
            Some("cancel while running"),
        )
        .await
        .unwrap();
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
async fn expired_sandbox_cancellation_is_terminal_and_cannot_fake_worker_acknowledgement() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let parent_id = AgentRunId::foreground_for_chat(chat.id);

    let expired_before_request = AgentRunId::new();
    store
        .accept_agent_run(
            expired_before_request,
            chat.id,
            Some(parent_id),
            Some(CallId::new()),
            AgentRunExecution::Sandbox,
            Some("cancel after lease expiry"),
        )
        .await
        .unwrap();
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

    let expired_after_request = AgentRunId::new();
    store
        .accept_agent_run(
            expired_after_request,
            chat.id,
            Some(parent_id),
            Some(CallId::new()),
            AgentRunExecution::Sandbox,
            Some("expire after cancellation request"),
        )
        .await
        .unwrap();
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
    assert!(store
        .finish_agent_run_cancellation(expired_after_request, lease_token)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn unavailable_parent_rejects_result_without_holding_the_scheduler_lock() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    let other_chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    store.create_chat(&other_chat).await.unwrap();
    let child_id = AgentRunId::new();
    store
        .accept_agent_run(
            child_id,
            chat.id,
            Some(AgentRunId::foreground_for_chat(chat.id)),
            Some(CallId::new()),
            AgentRunExecution::Sandbox,
            Some("parent may finish first"),
        )
        .await
        .unwrap();
    let lease_token = uuid::Uuid::new_v4();
    store
        .claim_agent_run(lease_token, Duration::minutes(1), 2, 1)
        .await
        .unwrap()
        .unwrap();
    let other_child_id = AgentRunId::new();
    store
        .accept_agent_run(
            other_child_id,
            other_chat.id,
            Some(AgentRunId::foreground_for_chat(other_chat.id)),
            Some(CallId::new()),
            AgentRunExecution::Sandbox,
            Some("scheduler remains usable"),
        )
        .await
        .unwrap();
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
        let run = match store
            .accept_agent_run(
                AgentRunId::new(),
                chat_id,
                Some(AgentRunId::foreground_for_chat(chat_id)),
                Some(CallId::new()),
                AgentRunExecution::Sandbox,
                Some(task),
            )
            .await
            .unwrap()
        {
            AcceptAgentRunOutcome::Accepted(run) => run,
            outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
        };
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
    let child_id = AgentRunId::new();
    let child = match store
        .accept_agent_run(
            child_id,
            chat.id,
            Some(AgentRunId::foreground_for_chat(chat.id)),
            Some(CallId::new()),
            AgentRunExecution::Sandbox,
            Some("deadline"),
        )
        .await
        .unwrap()
    {
        AcceptAgentRunOutcome::Accepted(run) => run,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    force_expired_agent_deadline(&store, child.id).await;
    assert!(store
        .claim_agent_run(uuid::Uuid::new_v4(), Duration::minutes(1), 1, 1)
        .await
        .unwrap()
        .is_none());
    let failed = store.get_agent_run(child_id).await.unwrap().unwrap();
    assert_eq!(failed.status, AgentRunStatus::Failed);
    assert_eq!(failed.last_error_code.as_deref(), Some("deadline_exceeded"));
}

#[tokio::test]
async fn expired_cancelling_sandbox_run_is_reaped_and_releases_capacity() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    let other_chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    store.create_chat(&other_chat).await.unwrap();
    let child_id = AgentRunId::new();
    store
        .accept_agent_run(
            child_id,
            chat.id,
            Some(AgentRunId::foreground_for_chat(chat.id)),
            Some(CallId::new()),
            AgentRunExecution::Sandbox,
            Some("cancellation reaper"),
        )
        .await
        .unwrap();
    store
        .claim_agent_run(uuid::Uuid::new_v4(), Duration::minutes(1), 2, 1)
        .await
        .unwrap()
        .unwrap();
    let other_child_id = AgentRunId::new();
    store
        .accept_agent_run(
            other_child_id,
            other_chat.id,
            Some(AgentRunId::foreground_for_chat(other_chat.id)),
            Some(CallId::new()),
            AgentRunExecution::Sandbox,
            Some("capacity-consuming live worker"),
        )
        .await
        .unwrap();
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
        let _accepted = match store
            .accept_agent_run(
                AgentRunId::new(),
                chat_id,
                Some(AgentRunId::foreground_for_chat(chat_id)),
                Some(CallId::new()),
                AgentRunExecution::Sandbox,
                Some("concurrent claim"),
            )
            .await
            .unwrap()
        {
            AcceptAgentRunOutcome::Accepted(run) => run,
            outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
        };
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
