use super::{sample_chat, temp_store};
use crate::agent_tools::{
    SandboxAgentFileResource, SpawnSandboxAgentResult, SPAWN_SANDBOX_AGENT_TOOL,
};
use crate::provider::ContentBlock;
use crate::{
    AgentEvent, AgentRun, AgentRunId, AgentRunStatus, CallId, ChatRootAttachment,
    CheckpointSandboxSpawnOutcome, HostRootId, ResolveToolCallOutcome, RootAttachmentChangeAction,
    RootAttachmentChangeId, RootAttachmentChangeTerminal, RootAttachmentOrigin,
    SandboxSpawnCheckpointRequest, Store, ToolCallExecution, ToolCallRecord, ToolCallStatus,
    TurnCheckpointProgress, TurnId, TurnRunStatus, Usage,
};
use chrono::{Duration, Utc};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

async fn running_turn(
    store: &crate::DbStore,
    chat_id: crate::ChatId,
) -> (crate::TurnRun, uuid::Uuid) {
    store
        .accept_turn(
            TurnId::new(),
            chat_id,
            "gpt-5",
            "coordinate background work",
        )
        .await
        .unwrap();
    let lease = uuid::Uuid::new_v4();
    let now = Utc::now();
    let turn = store
        .claim_turn_run(lease, now, now + Duration::minutes(5))
        .await
        .unwrap()
        .turn
        .expect("turn should claim");
    store
        .append_turn_event(
            chat_id,
            turn.id,
            lease,
            1,
            Utc::now(),
            &AgentEvent::TurnStarted { turn_id: turn.id },
        )
        .await
        .unwrap();
    (turn, lease)
}

fn request(
    turn: &crate::TurnRun,
    lease_token: uuid::Uuid,
    call_id: CallId,
    task: &str,
    ordinal: i32,
    progress: TurnCheckpointProgress,
) -> SandboxSpawnCheckpointRequest {
    SandboxSpawnCheckpointRequest {
        origin_turn_id: turn.id,
        lease_token,
        expected_steer_revision: turn.steer_revision,
        call_id,
        provider_id: format!("provider-{call_id}"),
        arguments: serde_json::json!({"task": task}),
        result: serde_json::to_string(&SpawnSandboxAgentResult {
            agent_id: AgentRunId::sandbox_for_spawn_call(call_id),
        })
        .unwrap(),
        event_ordinal: ordinal,
        progress,
        execution_location: crate::AgentRunExecutionLocation::InProcess,
    }
}

fn resource_request(
    turn: &crate::TurnRun,
    lease_token: uuid::Uuid,
    call_id: CallId,
    task: &str,
    resource: &SandboxAgentFileResource,
) -> SandboxSpawnCheckpointRequest {
    let mut request = request(turn, lease_token, call_id, task, 2, progress());
    request.arguments = serde_json::json!({
        "task": task,
        "resource": resource,
    });
    request
}

fn progress() -> TurnCheckpointProgress {
    TurnCheckpointProgress {
        model_steps: 1,
        usage: Usage {
            input_tokens: 11,
            output_tokens: 7,
            cache_read_input_tokens: 3,
            cache_creation_input_tokens: 2,
        },
    }
}

async fn detach_conversation_root(
    store: &crate::DbStore,
    chat_id: crate::ChatId,
    root_id: HostRootId,
) {
    let executor_id = uuid::Uuid::new_v4();
    let created_at =
        chrono::DateTime::from_timestamp_micros(Utc::now().timestamp_micros()).unwrap();
    let change_id = RootAttachmentChangeId::new();
    let revision = store
        .get_chat(chat_id)
        .await
        .unwrap()
        .unwrap()
        .attachment_revision;
    store
        .begin_root_attachment_change(&crate::BeginRootAttachmentChange {
            id: change_id,
            chat_id,
            executor_id,
            root_id,
            action: RootAttachmentChangeAction::Detach,
            expected_attachment_revision: revision,
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
            created_at + Duration::seconds(1),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn spawn_completion_requires_a_preceding_event_in_the_exact_claim() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    store
        .accept_turn(turn_id, chat.id, "gpt-5", "missing start event")
        .await
        .unwrap();
    let lease = uuid::Uuid::new_v4();
    let now = Utc::now();
    let turn = store
        .claim_turn_run(lease, now, now + Duration::minutes(5))
        .await
        .unwrap()
        .turn
        .unwrap();
    let request = request(
        &turn,
        lease,
        CallId::new(),
        "must not commit",
        2,
        progress(),
    );
    assert!(matches!(
        store
            .checkpoint_sandbox_spawn(&request, Utc::now())
            .await
            .unwrap(),
        Some(CheckpointSandboxSpawnOutcome::IdentityConflict)
    ));
    assert_eq!(store.list_agent_runs(chat.id).await.unwrap().len(), 1);
    assert!(store.list_tool_calls(chat.id).await.unwrap().is_empty());
    assert!(store.list_events(chat.id, 0).await.unwrap().is_empty());
}

#[tokio::test]
async fn nonblocking_spawn_commits_one_atomic_yield_and_exact_retry_survives_reclaim() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (running, lease) = running_turn(&store, chat.id).await;
    let request = request(
        &running,
        lease,
        CallId::new(),
        "research one fact",
        2,
        progress(),
    );

    let committed = store
        .checkpoint_sandbox_spawn(&request, Utc::now())
        .await
        .unwrap()
        .unwrap();
    let (child, yielded, call, checkpoint, event) = match committed {
        CheckpointSandboxSpawnOutcome::Checkpointed {
            child,
            turn,
            call,
            checkpoint,
            event,
        } => (child, turn, call, checkpoint, event),
        outcome => panic!("unexpected checkpoint outcome: {outcome:?}"),
    };
    assert_eq!(
        child.id,
        AgentRunId::sandbox_for_spawn_call(request.call_id)
    );
    assert_eq!(child.status, AgentRunStatus::Queued);
    assert_eq!(child.depth, AgentRun::MAX_DEPTH);
    assert_eq!(yielded.status, TurnRunStatus::Resuming);
    assert_eq!(yielded.lease_token, None);
    assert_eq!(yielded.lease_expires_at, None);
    assert_eq!(yielded.model_steps, running.model_steps + 1);
    assert_eq!(yielded.usage, progress().usage);
    assert_eq!(call.execution, ToolCallExecution::Orchestration);
    assert_eq!(call.status, ToolCallStatus::Completed);
    assert_eq!(checkpoint.event_seq, event.seq);
    assert!(matches!(
        event.event,
        AgentEvent::ToolCallCompleted { call_id, .. } if call_id == request.call_id
    ));

    let resumed_lease = uuid::Uuid::new_v4();
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    let future = Utc::now();
    let reclaimed = store
        .claim_turn_run(resumed_lease, future, future + Duration::minutes(5))
        .await
        .unwrap()
        .turn
        .expect("resuming turn should reclaim");
    assert_eq!(reclaimed.id, running.id);
    assert_eq!(reclaimed.status, TurnRunStatus::Running);
    assert_eq!(reclaimed.attempt_count, running.attempt_count);
    assert_eq!(reclaimed.claim_count, running.claim_count + 1);

    assert!(matches!(
        store
            .checkpoint_sandbox_spawn(&request, Utc::now())
            .await
            .unwrap(),
        Some(CheckpointSandboxSpawnOutcome::Existing { child: existing, checkpoint: receipt, .. })
            if existing == child && receipt == checkpoint
    ));
    assert_eq!(
        store.get_turn_run(running.id).await.unwrap(),
        Some(reclaimed),
        "an old exact retry must neither leak nor clear the new claim"
    );
    assert_eq!(store.list_agent_runs(chat.id).await.unwrap().len(), 2);
    assert_eq!(store.list_tool_calls(chat.id).await.unwrap(), vec![call]);
    assert_eq!(store.list_events(chat.id, 0).await.unwrap().len(), 2);
    assert_eq!(
        store
            .get_sandbox_agent_admission(child.id)
            .await
            .unwrap()
            .unwrap()
            .resource,
        None
    );
}

#[tokio::test]
async fn container_checkpoint_matches_standalone_container_admission_shape() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (turn, lease) = running_turn(&store, chat.id).await;
    let mut checkpoint_request = request(
        &turn,
        lease,
        CallId::new(),
        "research one container fact",
        2,
        progress(),
    );
    checkpoint_request.execution_location = crate::AgentRunExecutionLocation::Container;
    let checkpoint_child = match store
        .checkpoint_sandbox_spawn(&checkpoint_request, Utc::now())
        .await
        .unwrap()
        .unwrap()
    {
        CheckpointSandboxSpawnOutcome::Checkpointed { child, .. } => child,
        outcome => panic!("unexpected container checkpoint outcome: {outcome:?}"),
    };

    let (_standalone_dir, standalone_store) = temp_store().await;
    let standalone_chat = sample_chat();
    standalone_store
        .create_chat(&standalone_chat)
        .await
        .unwrap();
    let (standalone_turn, standalone_lease) =
        running_turn(&standalone_store, standalone_chat.id).await;
    let standalone_child = match standalone_store
        .admit_sandbox_container_agent_run(
            standalone_turn.id,
            CallId::new(),
            "research one container fact",
            standalone_lease,
            standalone_turn.steer_revision,
            AgentRun::MAX_CONCURRENCY_LIMIT,
            Utc::now(),
        )
        .await
        .unwrap()
        .unwrap()
    {
        crate::AdmitSandboxAgentRunOutcome::Accepted { child, .. } => child,
        outcome => panic!("unexpected standalone container admission: {outcome:?}"),
    };

    assert_eq!(
        checkpoint_child.execution_location,
        crate::AgentRunExecutionLocation::Container
    );
    assert_eq!(checkpoint_child.max_attempts, 1);
    assert_eq!(
        checkpoint_child.execution_location,
        standalone_child.execution_location
    );
    assert_eq!(checkpoint_child.input, standalone_child.input);
    assert_eq!(
        checkpoint_child.attempt_count,
        standalone_child.attempt_count
    );
    assert_eq!(checkpoint_child.max_attempts, standalone_child.max_attempts);
    assert_eq!(
        checkpoint_child.deadline_at.unwrap() - checkpoint_child.created_at,
        standalone_child.deadline_at.unwrap() - standalone_child.created_at
    );
}

#[tokio::test]
async fn exact_file_delegation_commits_with_admission_and_fences_retries() {
    let (_dir, store) = temp_store().await;
    let resource = SandboxAgentFileResource {
        root_id: HostRootId::from_uuid(uuid::Uuid::new_v4()).unwrap(),
        relative_path: "reports/summary.md".into(),
    };
    let mut chat = sample_chat();
    chat.attachment_revision = 1;
    chat.root_attachments.push(ChatRootAttachment {
        root_id: resource.root_id,
        origin: RootAttachmentOrigin::Conversation,
    });
    store.create_chat(&chat).await.unwrap();
    let (turn, lease) = running_turn(&store, chat.id).await;
    let request = resource_request(&turn, lease, CallId::new(), "inspect one report", &resource);

    let child = match store
        .checkpoint_sandbox_spawn(&request, Utc::now())
        .await
        .unwrap()
        .unwrap()
    {
        CheckpointSandboxSpawnOutcome::Checkpointed { child, .. } => child,
        outcome => panic!("unexpected resource spawn outcome: {outcome:?}"),
    };
    assert_eq!(
        store
            .get_sandbox_agent_admission(child.id)
            .await
            .unwrap()
            .unwrap()
            .resource,
        Some(resource.clone())
    );
    detach_conversation_root(&store, chat.id, resource.root_id).await;
    assert!(matches!(
        store
            .checkpoint_sandbox_spawn(&request, Utc::now())
            .await
            .unwrap(),
        Some(CheckpointSandboxSpawnOutcome::Existing { child: existing, .. }) if existing == child
    ));

    let mut changed = request;
    changed.arguments["resource"]["relative_path"] = serde_json::json!("reports/other.md");
    assert!(matches!(
        store
            .checkpoint_sandbox_spawn(&changed, Utc::now())
            .await
            .unwrap(),
        Some(CheckpointSandboxSpawnOutcome::IdentityConflict)
    ));
    assert_eq!(store.list_agent_runs(chat.id).await.unwrap().len(), 2);
    assert_eq!(store.list_tool_calls(chat.id).await.unwrap().len(), 1);
}

#[tokio::test]
async fn unattached_file_delegation_rejects_without_partial_writes() {
    let (_dir, store) = temp_store().await;
    let resource = SandboxAgentFileResource {
        root_id: HostRootId::from_uuid(uuid::Uuid::new_v4()).unwrap(),
        relative_path: "reports/missing.md".into(),
    };
    let mut chat = sample_chat();
    chat.attachment_revision = 1;
    chat.root_attachments.push(ChatRootAttachment {
        root_id: resource.root_id,
        origin: RootAttachmentOrigin::Conversation,
    });
    store.create_chat(&chat).await.unwrap();
    detach_conversation_root(&store, chat.id, resource.root_id).await;
    assert!(store
        .get_chat(chat.id)
        .await
        .unwrap()
        .unwrap()
        .root_attachments
        .is_empty());

    let (turn, lease) = running_turn(&store, chat.id).await;
    let request = resource_request(
        &turn,
        lease,
        CallId::new(),
        "inspect a detached report",
        &resource,
    );

    assert!(matches!(
        store
            .checkpoint_sandbox_spawn(&request, Utc::now())
            .await
            .unwrap(),
        Some(CheckpointSandboxSpawnOutcome::DelegatedResourceUnavailable)
    ));
    assert_eq!(store.list_agent_runs(chat.id).await.unwrap().len(), 1);
    assert!(store.list_tool_calls(chat.id).await.unwrap().is_empty());
    assert_eq!(store.list_events(chat.id, 0).await.unwrap().len(), 1);
    assert_eq!(
        store.get_turn_run(turn.id).await.unwrap().unwrap().status,
        TurnRunStatus::Running
    );
}

#[tokio::test]
async fn concurrent_detach_and_file_delegation_serialize_to_one_coherent_snapshot() {
    let (_dir, store) = temp_store().await;
    let resource = SandboxAgentFileResource {
        root_id: HostRootId::from_uuid(uuid::Uuid::new_v4()).unwrap(),
        relative_path: "reports/race.md".into(),
    };
    let mut chat = sample_chat();
    chat.attachment_revision = 1;
    chat.root_attachments.push(ChatRootAttachment {
        root_id: resource.root_id,
        origin: RootAttachmentOrigin::Conversation,
    });
    store.create_chat(&chat).await.unwrap();
    let (turn, lease) = running_turn(&store, chat.id).await;
    let request = resource_request(
        &turn,
        lease,
        CallId::new(),
        "inspect while the root detaches",
        &resource,
    );

    let executor_id = uuid::Uuid::new_v4();
    let change_id = RootAttachmentChangeId::new();
    let created_at =
        chrono::DateTime::from_timestamp_micros(Utc::now().timestamp_micros()).unwrap();
    store
        .begin_root_attachment_change(&crate::BeginRootAttachmentChange {
            id: change_id,
            chat_id: chat.id,
            executor_id,
            root_id: resource.root_id,
            action: RootAttachmentChangeAction::Detach,
            expected_attachment_revision: 1,
            created_at,
        })
        .await
        .unwrap();

    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    let spawn_store = store.clone();
    let spawn_barrier = barrier.clone();
    let spawn = tokio::spawn(async move {
        spawn_barrier.wait().await;
        spawn_store
            .checkpoint_sandbox_spawn(&request, Utc::now())
            .await
            .unwrap()
            .unwrap()
    });
    let detach_store = store.clone();
    let detach = tokio::spawn(async move {
        barrier.wait().await;
        detach_store
            .finish_root_attachment_change(
                change_id,
                executor_id,
                &RootAttachmentChangeTerminal::Completed {
                    broker_changed: true,
                    broker_currently_attached: false,
                },
                created_at + Duration::seconds(1),
            )
            .await
            .unwrap();
    });
    let (spawn, detach) = tokio::join!(spawn, detach);
    let spawn = spawn.expect("concurrent spawn task panicked");
    detach.expect("concurrent detach task panicked");

    assert!(store
        .get_chat(chat.id)
        .await
        .unwrap()
        .unwrap()
        .root_attachments
        .is_empty());
    match spawn {
        CheckpointSandboxSpawnOutcome::Checkpointed { child, .. } => {
            assert_eq!(
                store
                    .get_sandbox_agent_admission(child.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .resource,
                Some(resource)
            );
            assert_eq!(store.list_agent_runs(chat.id).await.unwrap().len(), 2);
            assert_eq!(store.list_tool_calls(chat.id).await.unwrap().len(), 1);
        }
        CheckpointSandboxSpawnOutcome::DelegatedResourceUnavailable => {
            assert_eq!(store.list_agent_runs(chat.id).await.unwrap().len(), 1);
            assert!(store.list_tool_calls(chat.id).await.unwrap().is_empty());
        }
        outcome => panic!("unexpected concurrent delegation outcome: {outcome:?}"),
    }
}

#[tokio::test]
async fn committed_spawn_identity_fences_every_model_and_accounting_field() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (turn, lease) = running_turn(&store, chat.id).await;
    let original = request(&turn, lease, CallId::new(), "original task", 2, progress());
    store
        .checkpoint_sandbox_spawn(&original, Utc::now())
        .await
        .unwrap();

    let mut variants = Vec::new();
    let mut changed = original.clone();
    changed.provider_id.push_str("-changed");
    variants.push(changed);
    let mut changed = original.clone();
    changed.arguments = serde_json::json!({"task": "changed task"});
    variants.push(changed);
    let mut changed = original.clone();
    changed.result.push(' ');
    variants.push(changed);
    let mut changed = original.clone();
    changed.event_ordinal += 1;
    variants.push(changed);
    let mut changed = original.clone();
    changed.progress.usage.output_tokens += 1;
    variants.push(changed);
    let mut changed = original.clone();
    changed.expected_steer_revision += 1;
    variants.push(changed);

    for changed in variants {
        assert!(matches!(
            store
                .checkpoint_sandbox_spawn(&changed, Utc::now())
                .await
                .unwrap(),
            Some(CheckpointSandboxSpawnOutcome::IdentityConflict)
        ));
    }
}

#[tokio::test]
async fn stale_lease_pending_steer_and_capacity_leave_no_checkpoint_fragments() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (turn, lease) = running_turn(&store, chat.id).await;

    let stale = request(
        &turn,
        uuid::Uuid::new_v4(),
        CallId::new(),
        "stale",
        2,
        progress(),
    );
    assert!(matches!(
        store
            .checkpoint_sandbox_spawn(&stale, Utc::now())
            .await
            .unwrap(),
        Some(CheckpointSandboxSpawnOutcome::LeaseLost)
    ));

    store
        .accept_turn_steer(
            crate::TurnSteerId::new(),
            turn.id,
            chat.id,
            "change direction",
            false,
        )
        .await
        .unwrap();
    let steered = request(&turn, lease, CallId::new(), "superseded", 3, progress());
    assert!(matches!(
        store
            .checkpoint_sandbox_spawn(&steered, Utc::now())
            .await
            .unwrap(),
        Some(CheckpointSandboxSpawnOutcome::SteerPending(_))
    ));
    assert_eq!(store.list_agent_runs(chat.id).await.unwrap().len(), 1);
    assert!(store.list_tool_calls(chat.id).await.unwrap().is_empty());
    assert_eq!(store.list_events(chat.id, 0).await.unwrap().len(), 1);

    // Use a fresh chat to exercise the fixed four-child outstanding cap.
    let cap_chat = sample_chat();
    store.create_chat(&cap_chat).await.unwrap();
    let (cap_turn, cap_lease) = running_turn(&store, cap_chat.id).await;
    for index in 0..AgentRun::DEFAULT_MAX_OUTSTANDING_CHILDREN {
        let call = CallId::new();
        assert!(matches!(
            store
                .admit_sandbox_agent_run(
                    cap_turn.id,
                    call,
                    &format!("child {index}"),
                    cap_lease,
                    cap_turn.steer_revision,
                    AgentRun::DEFAULT_MAX_OUTSTANDING_CHILDREN,
                    Utc::now(),
                )
                .await
                .unwrap(),
            Some(crate::AdmitSandboxAgentRunOutcome::Accepted { .. })
        ));
    }
    let over_cap = request(
        &cap_turn,
        cap_lease,
        CallId::new(),
        "fifth child",
        2,
        progress(),
    );
    assert!(matches!(
        store
            .checkpoint_sandbox_spawn(&over_cap, Utc::now())
            .await
            .unwrap(),
        Some(CheckpointSandboxSpawnOutcome::AtCapacity)
    ));
    assert_eq!(store.list_agent_runs(cap_chat.id).await.unwrap().len(), 5);
    assert!(store.list_tool_calls(cap_chat.id).await.unwrap().is_empty());
}

#[tokio::test]
async fn accounting_overflow_and_event_ordinal_collision_roll_back_every_write() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (turn, lease) = running_turn(&store, chat.id).await;
    let skipped = request(&turn, lease, CallId::new(), "skip ordinal", 3, progress());
    assert!(matches!(
        store
            .checkpoint_sandbox_spawn(&skipped, Utc::now())
            .await
            .unwrap(),
        Some(CheckpointSandboxSpawnOutcome::IdentityConflict)
    ));
    assert_eq!(store.list_agent_runs(chat.id).await.unwrap().len(), 1);
    let collision_ordinal = 2;
    store
        .append_turn_event(
            chat.id,
            turn.id,
            lease,
            collision_ordinal,
            Utc::now(),
            &AgentEvent::ReasoningDelta {
                text: "prior".into(),
            },
        )
        .await
        .unwrap();
    let collision = request(
        &turn,
        lease,
        CallId::new(),
        "must roll back",
        collision_ordinal,
        progress(),
    );
    assert!(matches!(
        store
            .checkpoint_sandbox_spawn(&collision, Utc::now())
            .await
            .unwrap(),
        Some(CheckpointSandboxSpawnOutcome::IdentityConflict)
    ));
    assert_eq!(store.list_agent_runs(chat.id).await.unwrap().len(), 1);
    assert!(store.list_tool_calls(chat.id).await.unwrap().is_empty());

    crate::db::entities::turn_run::Entity::update_many()
        .col_expr(
            crate::db::entities::turn_run::Column::ModelSteps,
            sea_orm::sea_query::Expr::value(i32::MAX),
        )
        .filter(crate::db::entities::turn_run::Column::Id.eq(turn.id.0))
        .exec(&store.conn)
        .await
        .unwrap();
    let overflow = request(&turn, lease, CallId::new(), "overflow", 3, progress());
    assert!(store
        .checkpoint_sandbox_spawn(&overflow, Utc::now())
        .await
        .is_err());
    assert_eq!(store.list_agent_runs(chat.id).await.unwrap().len(), 1);
    assert!(store.list_tool_calls(chat.id).await.unwrap().is_empty());

    crate::db::entities::turn_run::Entity::update_many()
        .col_expr(
            crate::db::entities::turn_run::Column::ModelSteps,
            sea_orm::sea_query::Expr::value(0),
        )
        .col_expr(
            crate::db::entities::turn_run::Column::InputTokens,
            sea_orm::sea_query::Expr::value(i64::from(u32::MAX)),
        )
        .filter(crate::db::entities::turn_run::Column::Id.eq(turn.id.0))
        .exec(&store.conn)
        .await
        .unwrap();
    let usage_overflow = request(&turn, lease, CallId::new(), "usage overflow", 3, progress());
    assert!(store
        .checkpoint_sandbox_spawn(&usage_overflow, Utc::now())
        .await
        .is_err());
    assert_eq!(store.list_agent_runs(chat.id).await.unwrap().len(), 1);
    assert!(store.list_tool_calls(chat.id).await.unwrap().is_empty());
}

#[tokio::test]
async fn completed_spawn_graph_does_not_block_quiescent_chat_deletion() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (turn, lease) = running_turn(&store, chat.id).await;
    let spawn = request(&turn, lease, CallId::new(), "delete later", 2, progress());
    store
        .checkpoint_sandbox_spawn(&spawn, Utc::now())
        .await
        .unwrap()
        .unwrap();
    store
        .request_agent_run_cancellation(AgentRunId::sandbox_for_spawn_call(spawn.call_id))
        .await
        .unwrap();
    store
        .request_turn_cancellation(turn.id, Utc::now() + Duration::seconds(1))
        .await
        .unwrap();
    assert_eq!(
        store.delete_chat(chat.id).await.unwrap(),
        crate::DeleteChatOutcome::Deleted
    );
}

#[tokio::test]
async fn orchestration_calls_are_not_generic_work_and_rebuild_once_in_spawn_order() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (first_turn, first_lease) = running_turn(&store, chat.id).await;
    let first = request(
        &first_turn,
        first_lease,
        CallId::new(),
        "first",
        2,
        progress(),
    );
    let first_call = match store
        .checkpoint_sandbox_spawn(&first, Utc::now())
        .await
        .unwrap()
        .unwrap()
    {
        CheckpointSandboxSpawnOutcome::Checkpointed { call, .. } => call,
        outcome => panic!("unexpected checkpoint outcome: {outcome:?}"),
    };
    assert_eq!(
        store
            .resolve_server_tool_call(
                first_call.id,
                &crate::ToolCallResolution::Completed {
                    result: "bad".into()
                },
                Utc::now(),
            )
            .await
            .unwrap(),
        ResolveToolCallOutcome::LeaseLost
    );
    let pending_orchestration = ToolCallRecord {
        id: CallId::new(),
        chat_id: chat.id,
        turn_id: first_turn.id,
        provider_id: "forbidden".into(),
        name: SPAWN_SANDBOX_AGENT_TOOL.into(),
        arguments: serde_json::json!({"task": "forbidden"}),
        execution: ToolCallExecution::Orchestration,
        status: ToolCallStatus::Pending,
        result: None,
        result_preview: None,
        error_code: None,
        error_detail: None,
        client_executor_id: None,
        client_lease_expires_at: None,
        created_at: Utc::now(),
        resolved_at: None,
    };
    assert!(store
        .accept_tool_call(&pending_orchestration)
        .await
        .is_err());

    let second_lease = uuid::Uuid::new_v4();
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    let future = Utc::now();
    let second_turn = store
        .claim_turn_run(second_lease, future, future + Duration::minutes(5))
        .await
        .unwrap()
        .turn
        .unwrap();
    store
        .append_turn_event(
            chat.id,
            second_turn.id,
            second_lease,
            1,
            Utc::now(),
            &AgentEvent::TurnStarted {
                turn_id: second_turn.id,
            },
        )
        .await
        .unwrap();
    let second = request(
        &second_turn,
        second_lease,
        CallId::new(),
        "second",
        2,
        progress(),
    );
    let second_call = match store
        .checkpoint_sandbox_spawn(&second, Utc::now())
        .await
        .unwrap()
        .unwrap()
    {
        CheckpointSandboxSpawnOutcome::Checkpointed { call, .. } => call,
        outcome => panic!("unexpected checkpoint outcome: {outcome:?}"),
    };
    let calls = store.list_tool_calls(chat.id).await.unwrap();
    assert_eq!(second_call.id, second.call_id);
    assert_eq!(
        calls.iter().map(|call| call.id).collect::<Vec<_>>(),
        vec![first.call_id, second.call_id]
    );
    let messages = store.list_messages(chat.id).await.unwrap();
    let transcript = crate::agent::rebuild_transcript_for_test(&messages, &calls, &[]);
    assert_eq!(
        transcript
            .iter()
            .filter(|message| message
                .content
                .iter()
                .any(|block| matches!(block, ContentBlock::ToolUse { .. })))
            .count(),
        2
    );
    assert_eq!(
        transcript
            .iter()
            .filter(|message| message
                .content
                .iter()
                .any(|block| matches!(block, ContentBlock::ToolResult { .. })))
            .count(),
        2
    );
    let tool_uses = transcript
        .iter()
        .flat_map(|message| &message.content)
        .filter_map(|block| match block {
            ContentBlock::ToolUse { id, .. } => Some(id.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let tool_results = transcript
        .iter()
        .flat_map(|message| &message.content)
        .filter_map(|block| match block {
            ContentBlock::ToolResult { tool_use_id, .. } => Some(tool_use_id.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        tool_uses,
        vec![first.provider_id.as_str(), second.provider_id.as_str()]
    );
    assert_eq!(tool_results, tool_uses);
}

#[tokio::test]
async fn tool_history_order_is_independent_of_provider_clock_skew() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let future = Utc::now() + Duration::days(1);
    let ordinary = ToolCallRecord {
        id: CallId::new(),
        chat_id: chat.id,
        turn_id: TurnId::new(),
        provider_id: "ordinary-before".into(),
        name: "read_file".into(),
        arguments: serde_json::json!({"path": "before"}),
        execution: ToolCallExecution::Server,
        status: ToolCallStatus::Pending,
        result: None,
        result_preview: None,
        error_code: None,
        error_detail: None,
        client_executor_id: None,
        client_lease_expires_at: None,
        created_at: future,
        resolved_at: None,
    };
    store.accept_tool_call(&ordinary).await.unwrap();
    store
        .resolve_server_tool_call(
            ordinary.id,
            &crate::ToolCallResolution::Completed {
                result: "ordinary result".into(),
            },
            future,
        )
        .await
        .unwrap();

    let (turn, lease) = running_turn(&store, chat.id).await;
    let spawn = request(&turn, lease, CallId::new(), "later history", 2, progress());
    let checkpoint = match store
        .checkpoint_sandbox_spawn(&spawn, Utc::now())
        .await
        .unwrap()
        .unwrap()
    {
        CheckpointSandboxSpawnOutcome::Checkpointed { checkpoint, .. } => checkpoint,
        outcome => panic!("unexpected checkpoint outcome: {outcome:?}"),
    };
    assert_eq!(checkpoint.history_order, 2);
    let calls = store.list_tool_calls(chat.id).await.unwrap();
    assert_eq!(
        calls.iter().map(|call| call.id).collect::<Vec<_>>(),
        vec![ordinary.id, spawn.call_id]
    );
    assert!(calls[0].created_at > calls[1].created_at);
}
