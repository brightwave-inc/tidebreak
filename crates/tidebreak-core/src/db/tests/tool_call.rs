use super::*;

#[tokio::test]
async fn server_tool_call_lifecycle_is_atomic_and_idempotent() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();

    let created = DateTime::<Utc>::from_timestamp(1_700_000_010, 0).unwrap();
    let call = ToolCallRecord {
        id: CallId::new(),
        chat_id: chat.id,
        turn_id: TurnId::new(),
        provider_id: "tu_1".into(),
        name: "read_file".into(),
        arguments: serde_json::json!({"path": "note.txt"}),
        raw_arguments: None,
        execution: ToolCallExecution::Server,
        status: ToolCallStatus::Pending,
        result: None,
        result_preview: None,

        provider_replay: None,
        error_code: None,
        error_detail: None,
        client_executor_id: None,
        client_lease_expires_at: None,
        created_at: created,
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
    assert_eq!(
        store
            .accept_tool_call(&ToolCallRecord {
                arguments: serde_json::json!({"path": "other.txt"}),
                raw_arguments: None,
                ..call.clone()
            })
            .await
            .unwrap(),
        AcceptToolCallOutcome::IdentityConflict
    );

    let completed = DateTime::<Utc>::from_timestamp(1_700_000_011, 0).unwrap();
    let resolution = ToolCallResolution::Completed {
        result: "hello".into(),
    };
    assert_eq!(
        store
            .resolve_server_tool_call(call.id, &resolution, completed)
            .await
            .unwrap(),
        ResolveToolCallOutcome::Resolved
    );
    assert_eq!(
        store
            .resolve_server_tool_call(call.id, &resolution, completed)
            .await
            .unwrap(),
        ResolveToolCallOutcome::Existing
    );
    match store.accept_tool_call(&call).await.unwrap() {
        AcceptToolCallOutcome::Existing(existing) => {
            assert_eq!(existing.status, ToolCallStatus::Completed);
            assert_eq!(existing.result.as_deref(), Some("hello"));
        }
        outcome => panic!("unexpected terminal acceptance retry: {outcome:?}"),
    }
    assert_eq!(
        store
            .resolve_server_tool_call(
                call.id,
                &ToolCallResolution::Completed {
                    result: "different".into(),
                },
                completed,
            )
            .await
            .unwrap(),
        ResolveToolCallOutcome::AlreadyTerminal
    );

    let listed = store.list_tool_calls(chat.id).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].created_at, created);
    assert_eq!(listed[0].resolved_at, Some(completed));
    assert_eq!(listed[0].status, ToolCallStatus::Completed);
    assert_eq!(listed[0].result.as_deref(), Some("hello"));
    assert_eq!(listed[0].arguments, serde_json::json!({"path": "note.txt"}));
}

#[tokio::test]
async fn claimed_tool_results_are_co_committed_with_the_turn_lease() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    let accepted = match store
        .accept_turn(turn_id, chat.id, "gpt-5", "run a tool")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected turn acceptance: {outcome:?}"),
    };
    let first_claim_at = accepted.available_at;
    let first_lease = uuid::Uuid::new_v4();
    store
        .claim_turn_run(
            first_lease,
            first_claim_at,
            first_claim_at + chrono::Duration::seconds(1),
        )
        .await
        .unwrap();
    let call = ToolCallRecord {
        id: CallId::new(),
        chat_id: chat.id,
        turn_id,
        provider_id: "tu_claimed".into(),
        name: "write_file".into(),
        arguments: serde_json::json!({"path": "note.txt"}),
        raw_arguments: None,
        execution: ToolCallExecution::Server,
        status: ToolCallStatus::Pending,
        result: None,
        result_preview: None,

        provider_replay: None,
        error_code: None,
        error_detail: None,
        client_executor_id: None,
        client_lease_expires_at: None,
        created_at: first_claim_at,
        resolved_at: None,
    };
    assert!(matches!(
        store
            .accept_claimed_tool_call(&call, first_lease, first_claim_at)
            .await
            .unwrap(),
        AcceptClaimedToolCallOutcome::Accepted(_)
    ));
    let stored = entities::tool_call::Entity::find_by_id(call.id.0)
        .one(&store.conn)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.turn_lease_token, Some(first_lease));
    assert_eq!(stored.resolution_turn_lease_token, None);

    let resolution = ToolCallResolution::Completed {
        result: "written".into(),
    };
    assert_eq!(
        store
            .resolve_claimed_server_tool_call(
                call.id,
                chat.id,
                turn_id,
                uuid::Uuid::new_v4(),
                first_claim_at,
                &resolution,
                first_claim_at,
            )
            .await
            .unwrap(),
        ResolveToolCallOutcome::LeaseLost
    );
    assert_eq!(
        store.list_tool_calls(chat.id).await.unwrap()[0].status,
        ToolCallStatus::Pending
    );

    let retry_at = first_claim_at + chrono::Duration::seconds(2);
    let retry_lease = uuid::Uuid::new_v4();
    let retried = store
        .claim_turn_run(
            retry_lease,
            retry_at,
            retry_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap()
        .turn
        .unwrap();
    assert_eq!(retried.attempt_count, 2);
    assert_eq!(
        store
            .resolve_claimed_server_tool_call(
                call.id,
                chat.id,
                turn_id,
                first_lease,
                retry_at,
                &resolution,
                retry_at,
            )
            .await
            .unwrap(),
        ResolveToolCallOutcome::LeaseLost,
        "the result and stale lease check must commit together"
    );

    let interrupted = ToolCallResolution::Failed {
        result: "not replayed".into(),
        error_code: "tool_execution_interrupted".into(),
        error_detail: None,
    };
    assert_eq!(
        store
            .abandon_inherited_server_tool_call(
                call.id,
                chat.id,
                turn_id,
                retry_lease,
                retry_at,
                &interrupted,
                retry_at,
            )
            .await
            .unwrap(),
        ResolveToolCallOutcome::Resolved
    );
    let stored = entities::tool_call::Entity::find_by_id(call.id.0)
        .one(&store.conn)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.resolution_turn_lease_token, Some(retry_lease));
    assert_eq!(stored.status, ToolCallStatus::Failed.as_str());
}

#[tokio::test]
async fn claimed_intermediate_message_is_co_committed_with_the_turn_lease() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    let accepted = match store
        .accept_turn(turn_id, chat.id, "gpt-5", "draft")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected turn acceptance: {outcome:?}"),
    };
    let claimed_at = accepted.available_at;
    let lease = uuid::Uuid::new_v4();
    store
        .claim_turn_run(lease, claimed_at, claimed_at + chrono::Duration::seconds(1))
        .await
        .unwrap();
    let message = Message {
        id: MessageId::new(),
        chat_id: chat.id,
        turn_id,
        role: Role::Assistant,
        reasoning: Default::default(),
        content: "intermediate".into(),
        llm_content: None,
        created_at: claimed_at,
    };
    assert_eq!(
        store
            .append_claimed_assistant_message_with_citations(&message, &[], lease, claimed_at,)
            .await
            .unwrap(),
        AppendClaimedMessageOutcome::Appended
    );
    assert_eq!(
        store
            .append_claimed_assistant_message_with_citations(&message, &[], lease, claimed_at,)
            .await
            .unwrap(),
        AppendClaimedMessageOutcome::Existing
    );

    let retry_at = claimed_at + chrono::Duration::seconds(2);
    let retry_lease = uuid::Uuid::new_v4();
    store
        .claim_turn_run(
            retry_lease,
            retry_at,
            retry_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap();
    let stale_message = Message {
        id: MessageId::new(),
        content: "stale".into(),
        llm_content: None,
        created_at: retry_at,
        ..message
    };
    assert_eq!(
        store
            .append_claimed_assistant_message_with_citations(&stale_message, &[], lease, retry_at,)
            .await
            .unwrap(),
        AppendClaimedMessageOutcome::LeaseLost
    );
    assert!(entities::message::Entity::find_by_id(stale_message.id.0)
        .one(&store.conn)
        .await
        .unwrap()
        .is_none());
}
