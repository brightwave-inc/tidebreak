use super::{sample_chat, temp_store};
use crate::agent_tools::{
    SandboxAgentFileResource, SpawnSandboxAgentResult, SANDBOX_READ_DELEGATED_FILE_TOOL,
};
use crate::{
    AgentEvent, AgentRun, AgentRunId, AgentRunStatus, CallId, ChatRootAttachment,
    CheckpointSandboxSpawnOutcome, ClaimDelegatedFileReadOutcome, ClaimSandboxToolCallOutcome,
    HostRootId, ParkSandboxToolCallOutcome, RootAttachmentChangeAction, RootAttachmentChangeId,
    RootAttachmentChangeTerminal, RootAttachmentOrigin, SandboxSpawnCheckpointRequest,
    SandboxToolCallRequest, Store, ToolCallResolution, TurnCheckpointProgress, TurnId, Usage,
};
use chrono::{Duration, Utc};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

async fn delegated_sandbox(
    store: &crate::DbStore,
) -> (crate::Chat, AgentRun, uuid::Uuid, SandboxAgentFileResource) {
    let root_id = HostRootId::from_uuid(uuid::Uuid::new_v4()).unwrap();
    let resource = SandboxAgentFileResource {
        root_id,
        relative_path: "reports/summary.md".into(),
    };
    let mut chat = sample_chat();
    chat.attachment_revision = 1;
    chat.root_attachments.push(ChatRootAttachment {
        root_id,
        origin: RootAttachmentOrigin::Conversation,
    });
    store.create_chat(&chat).await.unwrap();
    store
        .accept_turn(
            TurnId::new(),
            chat.id,
            "test-model",
            "read one delegated file",
        )
        .await
        .unwrap();
    let turn_lease = uuid::Uuid::new_v4();
    let now = Utc::now();
    let turn = store
        .claim_turn_run(turn_lease, now, now + Duration::minutes(5))
        .await
        .unwrap()
        .turn
        .unwrap();
    store
        .append_turn_event(
            chat.id,
            turn.id,
            turn_lease,
            1,
            Utc::now(),
            &AgentEvent::TurnStarted { turn_id: turn.id },
        )
        .await
        .unwrap();
    let spawn_call_id = CallId::new();
    let child_id = AgentRunId::sandbox_for_spawn_call(spawn_call_id);
    let request = SandboxSpawnCheckpointRequest {
        origin_turn_id: turn.id,
        lease_token: turn_lease,
        expected_steer_revision: turn.steer_revision,
        call_id: spawn_call_id,
        provider_id: "spawn-provider-call".into(),
        arguments: serde_json::json!({
            "task": "read the delegated report",
            "resource": resource,
        }),
        approval_gated: false,
        result: serde_json::to_string(&SpawnSandboxAgentResult { agent_id: child_id }).unwrap(),
        event_ordinal: 2,
        progress: TurnCheckpointProgress {
            model_steps: 1,
            usage: Usage::default(),
        },
        remaining_requests: Vec::new(),
        max_active_background_agents: AgentRun::DEFAULT_MAX_ACTIVE_BACKGROUND_AGENTS,
        execution_location: crate::AgentRunExecutionLocation::InProcess,
    };
    let child = match store
        .checkpoint_sandbox_spawn(&request, Utc::now())
        .await
        .unwrap()
        .unwrap()
    {
        CheckpointSandboxSpawnOutcome::Checkpointed { child, .. } => child,
        outcome => panic!("unexpected spawn checkpoint: {outcome:?}"),
    };
    let worker_lease = uuid::Uuid::new_v4();
    assert_eq!(
        store
            .claim_agent_run(worker_lease, Duration::minutes(5), 1, 1)
            .await
            .unwrap()
            .unwrap()
            .id,
        child.id
    );
    (chat, child, worker_lease, resource)
}

fn read_request(run: &AgentRun, id: CallId) -> SandboxToolCallRequest {
    SandboxToolCallRequest {
        id,
        agent_run_id: run.id,
        chat_id: run.chat_id,
        provider_id: "delegated-read-provider-call".into(),
        name: SANDBOX_READ_DELEGATED_FILE_TOOL.into(),
        arguments: serde_json::json!({}),
    }
}

async fn detach_root(store: &crate::DbStore, chat: &crate::Chat, root_id: HostRootId) {
    let executor_id = uuid::Uuid::new_v4();
    let change_id = RootAttachmentChangeId::new();
    let created_at =
        chrono::DateTime::from_timestamp_micros(Utc::now().timestamp_micros()).unwrap();
    store
        .begin_root_attachment_change(&crate::BeginRootAttachmentChange {
            id: change_id,
            chat_id: chat.id,
            executor_id,
            root_id,
            action: RootAttachmentChangeAction::Detach,
            expected_attachment_revision: 1,
            created_at,
        })
        .await
        .unwrap();
    store
        .finish_root_attachment_change(
            change_id,
            executor_id,
            &RootAttachmentChangeTerminal::Completed {
                broker_changed: true,
                broker_currently_attached: false,
            },
            Utc::now(),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn delegated_read_requires_exact_admission_and_empty_arguments() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let run = super::agent_run::admit_sandbox_call_for_test(
        &store,
        chat.id,
        CallId::new(),
        "no delegated file",
    )
    .await;
    let worker_lease = uuid::Uuid::new_v4();
    store
        .claim_agent_run(worker_lease, Duration::minutes(5), 1, 1)
        .await
        .unwrap();
    let request = read_request(&run, CallId::new());
    assert_eq!(
        store
            .park_agent_run_for_sandbox_tool_calls(
                run.id,
                worker_lease,
                &[super::dispatchable(&request)]
            )
            .await
            .unwrap(),
        ParkSandboxToolCallOutcome::DelegatedResourceUnavailable
    );
    assert!(store
        .list_sandbox_tool_calls_for_agent_run(run.id)
        .await
        .unwrap()
        .is_empty());
    let web = SandboxToolCallRequest {
        id: CallId::new(),
        agent_run_id: run.id,
        chat_id: chat.id,
        provider_id: "web-provider-call".into(),
        name: "web_search".into(),
        arguments: serde_json::json!({"query": "isolation"}),
    };
    assert!(matches!(
        store
            .park_agent_run_for_sandbox_tool_calls(
                run.id,
                worker_lease,
                &[super::dispatchable(&web)]
            )
            .await
            .unwrap(),
        ParkSandboxToolCallOutcome::Parked { .. }
    ));
    assert_eq!(
        store
            .claim_delegated_file_read(web.id, uuid::Uuid::new_v4(), Duration::minutes(1))
            .await
            .unwrap(),
        ClaimDelegatedFileReadOutcome::Unavailable
    );

    let (_second_dir, second_store) = temp_store().await;
    let (_chat, run, worker_lease, _resource) = delegated_sandbox(&second_store).await;
    for arguments in [
        serde_json::json!({"path": "other.txt"}),
        serde_json::Value::Null,
    ] {
        let mut malformed = read_request(&run, CallId::new());
        malformed.arguments = arguments;
        assert!(second_store
            .park_agent_run_for_sandbox_tool_calls(
                run.id,
                worker_lease,
                &[super::dispatchable(&malformed)]
            )
            .await
            .is_err());
    }

    let (_third_dir, third_store) = temp_store().await;
    let (_chat, corrupt_run, corrupt_lease, _resource) = delegated_sandbox(&third_store).await;
    crate::db::entities::agent_run::Entity::update_many()
        .col_expr(
            crate::db::entities::agent_run::Column::DelegatedRelativePath,
            sea_orm::sea_query::Expr::value(Some("../escaped.txt".to_owned())),
        )
        .filter(crate::db::entities::agent_run::Column::Id.eq(corrupt_run.id.0))
        .exec(&third_store.conn)
        .await
        .unwrap();
    assert_eq!(
        third_store
            .park_agent_run_for_sandbox_tool_calls(
                corrupt_run.id,
                corrupt_lease,
                &[super::dispatchable(&read_request(
                    &corrupt_run,
                    CallId::new()
                ))],
            )
            .await
            .unwrap(),
        ParkSandboxToolCallOutcome::DelegatedResourceUnavailable
    );
}

#[tokio::test]
async fn delegated_read_checkpoint_and_claim_are_exact_and_lane_isolated() {
    let (_dir, store) = temp_store().await;
    let (chat, run, worker_lease, resource) = delegated_sandbox(&store).await;
    let request = read_request(&run, CallId::new());
    assert!(matches!(
        store
            .park_agent_run_for_sandbox_tool_calls(
                run.id,
                worker_lease,
                &[super::dispatchable(&request)]
            )
            .await
            .unwrap(),
        ParkSandboxToolCallOutcome::Parked { .. }
    ));
    assert!(matches!(
        store
            .park_agent_run_for_sandbox_tool_calls(
                run.id,
                worker_lease,
                &[super::dispatchable(&request)]
            )
            .await
            .unwrap(),
        ParkSandboxToolCallOutcome::Existing { .. }
    ));
    assert!(store
        .list_sandbox_tool_call_candidates_named("web_search", 8)
        .await
        .unwrap()
        .is_empty());
    assert!(matches!(
        store
            .claim_sandbox_tool_call_named(
                request.id,
                "web_search",
                uuid::Uuid::new_v4(),
                Duration::minutes(1),
            )
            .await
            .unwrap(),
        ClaimSandboxToolCallOutcome::Unavailable
    ));
    let lease = uuid::Uuid::new_v4();
    let claim = match store
        .claim_delegated_file_read(request.id, lease, Duration::minutes(1))
        .await
        .unwrap()
    {
        ClaimDelegatedFileReadOutcome::Claimed(claim) => claim,
        outcome => panic!("unexpected delegated read claim: {outcome:?}"),
    };
    assert_eq!(claim.call.chat_id, chat.id);
    assert_eq!(claim.root_id, resource.root_id);
    assert_eq!(claim.relative_path, resource.relative_path);
    assert!(matches!(
        store
            .claim_delegated_file_read(request.id, lease, Duration::minutes(1))
            .await
            .unwrap(),
        ClaimDelegatedFileReadOutcome::Existing(existing) if existing == claim
    ));
    let resolution = ToolCallResolution::Completed {
        result: serde_json::json!({"content": "bounded UTF-8 text"}).to_string(),
    };
    assert_eq!(
        store
            .resolve_delegated_file_read(request.id, lease, &resolution)
            .await
            .unwrap(),
        crate::ResolveSandboxToolCallOutcome::Resolved
    );
    assert_eq!(
        store
            .resolve_delegated_file_read(request.id, lease, &resolution)
            .await
            .unwrap(),
        crate::ResolveSandboxToolCallOutcome::Existing
    );
    let receipt = store
        .get_sandbox_tool_call_receipt(request.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(receipt.result, resolution.result());
    assert!(!receipt.result.contains(&resource.relative_path));
    assert!(!receipt.result.contains(&resource.root_id.to_string()));

    let next_worker_lease = uuid::Uuid::new_v4();
    assert_eq!(
        store
            .claim_agent_run(next_worker_lease, Duration::minutes(1), 1, 1)
            .await
            .unwrap()
            .unwrap()
            .id,
        run.id
    );
    let second = SandboxToolCallRequest {
        id: CallId::new(),
        agent_run_id: run.id,
        chat_id: chat.id,
        provider_id: "second-provider-call".into(),
        name: "web_search".into(),
        arguments: serde_json::json!({"query": "second tool"}),
    };
    assert!(
        matches!(
            store
                .park_agent_run_for_sandbox_tool_calls(
                    run.id,
                    next_worker_lease,
                    &[super::dispatchable(&second)]
                )
                .await
                .unwrap(),
            ParkSandboxToolCallOutcome::Parked { .. }
        ),
        "a resolved read does not end the run's checkpoint chain"
    );
    // The chain continues, but each checkpoint stays on its own executor lane:
    // the delegated-file broker never claims search work.
    assert!(matches!(
        store
            .claim_delegated_file_read(second.id, uuid::Uuid::new_v4(), Duration::minutes(1))
            .await
            .unwrap(),
        ClaimDelegatedFileReadOutcome::Unavailable
    ));
}

#[tokio::test]
async fn detach_before_claim_terminalizes_without_exposing_authority() {
    let (_dir, store) = temp_store().await;
    let (chat, run, worker_lease, resource) = delegated_sandbox(&store).await;
    let request = read_request(&run, CallId::new());
    store
        .park_agent_run_for_sandbox_tool_calls(
            run.id,
            worker_lease,
            &[super::dispatchable(&request)],
        )
        .await
        .unwrap();
    detach_root(&store, &chat, resource.root_id).await;

    assert_eq!(
        store
            .claim_delegated_file_read(request.id, uuid::Uuid::new_v4(), Duration::minutes(1),)
            .await
            .unwrap(),
        ClaimDelegatedFileReadOutcome::Unavailable
    );
    let receipt = store
        .get_sandbox_tool_call_receipt(request.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        receipt.error_code.as_deref(),
        Some("delegated_file_unavailable")
    );
    assert!(!receipt.result.contains(&resource.relative_path));
    assert!(!receipt.result.contains(&resource.root_id.to_string()));
    assert_eq!(
        store.get_agent_run(run.id).await.unwrap().unwrap().status,
        AgentRunStatus::RetryWait
    );
    assert!(crate::db::entities::sandbox_tool_call::Entity::find()
        .filter(crate::db::entities::sandbox_tool_call::Column::Id.eq(request.id.0))
        .one(&store.conn)
        .await
        .unwrap()
        .unwrap()
        .executor_lease_token
        .is_none());
}

#[tokio::test]
async fn concurrent_detach_and_claim_require_a_final_revocation_heartbeat() {
    let (_dir, store) = temp_store().await;
    let (chat, run, worker_lease, resource) = delegated_sandbox(&store).await;
    let request = read_request(&run, CallId::new());
    store
        .park_agent_run_for_sandbox_tool_calls(
            run.id,
            worker_lease,
            &[super::dispatchable(&request)],
        )
        .await
        .unwrap();

    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    let claim_store = store.clone();
    let claim_barrier = barrier.clone();
    let call_id = request.id;
    let lease = uuid::Uuid::new_v4();
    let claim = tokio::spawn(async move {
        claim_barrier.wait().await;
        claim_store
            .claim_delegated_file_read(call_id, lease, Duration::minutes(1))
            .await
            .unwrap()
    });
    let detach_store = store.clone();
    let detach_chat = chat.clone();
    let detach = tokio::spawn(async move {
        barrier.wait().await;
        detach_root(&detach_store, &detach_chat, resource.root_id).await;
    });
    let claim = claim.await.unwrap();
    detach.await.unwrap();

    assert!(matches!(
        claim,
        ClaimDelegatedFileReadOutcome::Claimed(_) | ClaimDelegatedFileReadOutcome::Unavailable
    ));
    assert_eq!(
        store
            .heartbeat_delegated_file_read(request.id, lease, Duration::minutes(1))
            .await
            .unwrap(),
        None,
        "the immediate pre-dispatch fence must observe the completed detach"
    );
    let receipt = store
        .get_sandbox_tool_call_receipt(request.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        receipt.error_code.as_deref(),
        Some("delegated_file_unavailable")
    );
    assert_eq!(
        store.get_agent_run(run.id).await.unwrap().unwrap().status,
        AgentRunStatus::RetryWait
    );
}

#[tokio::test]
async fn detach_after_claim_rejects_content_at_the_atomic_resolution_fence() {
    let (_dir, store) = temp_store().await;
    let (chat, run, worker_lease, resource) = delegated_sandbox(&store).await;
    let request = read_request(&run, CallId::new());
    store
        .park_agent_run_for_sandbox_tool_calls(
            run.id,
            worker_lease,
            &[super::dispatchable(&request)],
        )
        .await
        .unwrap();
    let lease = uuid::Uuid::new_v4();
    assert!(matches!(
        store
            .claim_delegated_file_read(request.id, lease, Duration::minutes(1))
            .await
            .unwrap(),
        ClaimDelegatedFileReadOutcome::Claimed(_)
    ));
    detach_root(&store, &chat, resource.root_id).await;

    let revoked_content = "content read from a now-revoked root";
    assert_eq!(
        store
            .resolve_delegated_file_read(
                request.id,
                lease,
                &ToolCallResolution::Completed {
                    result: revoked_content.into(),
                },
            )
            .await
            .unwrap(),
        crate::ResolveSandboxToolCallOutcome::LeaseLost
    );
    let receipt = store
        .get_sandbox_tool_call_receipt(request.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(receipt.status, crate::SandboxToolCallStatus::Failed);
    assert_eq!(
        receipt.error_code.as_deref(),
        Some("delegated_file_unavailable")
    );
    assert!(!receipt.result.contains(revoked_content));
    assert!(!receipt.result.contains(&resource.relative_path));
    assert!(!receipt.result.contains(&resource.root_id.to_string()));
    assert_eq!(
        store.get_agent_run(run.id).await.unwrap().unwrap().status,
        AgentRunStatus::RetryWait
    );
}

#[tokio::test]
async fn exact_terminal_retry_survives_a_later_attachment_revocation() {
    let (_dir, store) = temp_store().await;
    let (chat, run, worker_lease, resource) = delegated_sandbox(&store).await;
    let request = read_request(&run, CallId::new());
    store
        .park_agent_run_for_sandbox_tool_calls(
            run.id,
            worker_lease,
            &[super::dispatchable(&request)],
        )
        .await
        .unwrap();
    let lease = uuid::Uuid::new_v4();
    store
        .claim_delegated_file_read(request.id, lease, Duration::minutes(1))
        .await
        .unwrap();
    let resolution = ToolCallResolution::Completed {
        result: serde_json::json!({"content": "committed while attached"}).to_string(),
    };
    assert_eq!(
        store
            .resolve_delegated_file_read(request.id, lease, &resolution)
            .await
            .unwrap(),
        crate::ResolveSandboxToolCallOutcome::Resolved
    );
    detach_root(&store, &chat, resource.root_id).await;
    assert_eq!(
        store
            .resolve_delegated_file_read(request.id, lease, &resolution)
            .await
            .unwrap(),
        crate::ResolveSandboxToolCallOutcome::Existing
    );
}

#[tokio::test]
async fn wrong_and_expired_delegated_read_leases_cannot_heartbeat_or_resolve() {
    let (_dir, store) = temp_store().await;
    let (_chat, run, worker_lease, _resource) = delegated_sandbox(&store).await;
    let request = read_request(&run, CallId::new());
    store
        .park_agent_run_for_sandbox_tool_calls(
            run.id,
            worker_lease,
            &[super::dispatchable(&request)],
        )
        .await
        .unwrap();
    let lease = uuid::Uuid::new_v4();
    assert!(matches!(
        store
            .claim_delegated_file_read(request.id, lease, Duration::minutes(1))
            .await
            .unwrap(),
        ClaimDelegatedFileReadOutcome::Claimed(_)
    ));
    let wrong = uuid::Uuid::new_v4();
    assert_eq!(
        store
            .heartbeat_delegated_file_read(request.id, wrong, Duration::minutes(1))
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        store
            .resolve_delegated_file_read(
                request.id,
                wrong,
                &crate::ToolCallResolution::Completed {
                    result: "wrong lease".into(),
                },
            )
            .await
            .unwrap(),
        crate::ResolveSandboxToolCallOutcome::LeaseLost
    );
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
            .heartbeat_delegated_file_read(request.id, lease, Duration::minutes(1))
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        store
            .resolve_delegated_file_read(
                request.id,
                lease,
                &crate::ToolCallResolution::Completed {
                    result: "expired lease".into(),
                },
            )
            .await
            .unwrap(),
        crate::ResolveSandboxToolCallOutcome::LeaseLost
    );
}

#[tokio::test]
async fn cancelling_a_delegated_read_fences_native_execution_without_leaking_authority() {
    let (_dir, store) = temp_store().await;
    let (_chat, run, worker_lease, resource) = delegated_sandbox(&store).await;
    let request = read_request(&run, CallId::new());
    store
        .park_agent_run_for_sandbox_tool_calls(
            run.id,
            worker_lease,
            &[super::dispatchable(&request)],
        )
        .await
        .unwrap();
    let lease = uuid::Uuid::new_v4();
    assert!(matches!(
        store
            .claim_delegated_file_read(request.id, lease, Duration::minutes(1))
            .await
            .unwrap(),
        ClaimDelegatedFileReadOutcome::Claimed(_)
    ));
    assert!(matches!(
        store.request_agent_run_cancellation(run.id).await.unwrap(),
        Some(crate::RequestAgentRunCancellationOutcome::Cancelled(_))
    ));
    assert_eq!(
        store
            .heartbeat_delegated_file_read(request.id, lease, Duration::minutes(1))
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        store
            .resolve_delegated_file_read(
                request.id,
                lease,
                &ToolCallResolution::Completed {
                    result: "late bytes".into(),
                },
            )
            .await
            .unwrap(),
        crate::ResolveSandboxToolCallOutcome::AlreadyTerminal
    );
    let receipt = store
        .get_sandbox_tool_call_receipt(request.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(receipt.status, crate::SandboxToolCallStatus::Cancelled);
    assert!(!receipt.result.contains(&resource.relative_path));
    assert!(!receipt.result.contains(&resource.root_id.to_string()));
}
