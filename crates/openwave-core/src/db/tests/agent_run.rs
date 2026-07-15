use super::{sample_chat, temp_store};
use crate::{
    AcceptAgentRunOutcome, AgentRunExecution, AgentRunId, AgentRunStatus, CallId, ChatId, Store,
};
use sea_orm::{ActiveModelTrait, Set};

#[tokio::test]
async fn foreground_and_sandbox_runs_roundtrip_with_exact_idempotency() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();

    let foreground_id = AgentRunId::new();
    let foreground = match store
        .accept_agent_run(
            foreground_id,
            chat.id,
            None,
            None,
            AgentRunExecution::Foreground,
            None,
        )
        .await
        .unwrap()
    {
        AcceptAgentRunOutcome::Accepted(run) => run,
        outcome => panic!("unexpected foreground outcome: {outcome:?}"),
    };
    assert_eq!(foreground.id, foreground_id);
    assert_eq!(foreground.chat_id, chat.id);
    assert_eq!(foreground.parent_id, None);
    assert_eq!(foreground.depth, 0);
    assert_eq!(foreground.execution, AgentRunExecution::Foreground);
    assert_eq!(foreground.status, AgentRunStatus::Active);
    assert_eq!(foreground.input, None);

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
    let foreground_id = AgentRunId::new();
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
        AcceptAgentRunOutcome::Accepted(_)
    ));
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
    let foreground_id = AgentRunId::new();
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
        .unwrap();

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
    let parent_id = AgentRunId::new();
    store
        .accept_agent_run(
            parent_id,
            parent_chat.id,
            None,
            None,
            AgentRunExecution::Foreground,
            None,
        )
        .await
        .unwrap();

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
        created_at: Set(now),
        updated_at: Set(now),
    };
    assert!(malformed.insert(&store.conn).await.is_err());
}
