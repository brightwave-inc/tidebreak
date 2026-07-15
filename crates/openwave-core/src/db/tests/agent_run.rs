use super::{sample_chat, temp_store};
use crate::{
    AcceptAgentRunOutcome, AgentRun, AgentRunExecution, AgentRunId, AgentRunStatus, CallId, ChatId,
    DbStore, FinishAgentRunCancellationOutcome, RequestAgentRunCancellationOutcome, Store,
    SubmitAgentRunResultOutcome,
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
