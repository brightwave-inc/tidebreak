use super::*;

async fn claimed_sensitive_call(
    store: &DbStore,
    chat: &Chat,
) -> (TurnId, uuid::Uuid, ToolCallRecord, ApprovalRequest) {
    claimed_sensitive_call_named(store, chat, "search").await
}

async fn claimed_sensitive_call_named(
    store: &DbStore,
    chat: &Chat,
    tool_name: &str,
) -> (TurnId, uuid::Uuid, ToolCallRecord, ApprovalRequest) {
    claimed_sensitive_call_with(
        store,
        chat,
        tool_name,
        serde_json::json!({"query": "private"}),
    )
    .await
}

async fn claimed_sensitive_call_with(
    store: &DbStore,
    chat: &Chat,
    tool_name: &str,
    arguments: serde_json::Value,
) -> (TurnId, uuid::Uuid, ToolCallRecord, ApprovalRequest) {
    let turn_id = TurnId::new();
    let accepted = match store
        .accept_turn(turn_id, chat.id, "gpt-5", "search")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    entities::turn_run::Entity::update_many()
        .col_expr(
            entities::turn_run::Column::MaxAttempts,
            sea_orm::sea_query::Expr::value(2),
        )
        .filter(entities::turn_run::Column::Id.eq(turn_id.0))
        .exec(&store.conn)
        .await
        .unwrap();
    let claimed_at = super::ops::agent_run::database_now(&store.conn)
        .await
        .unwrap()
        .max(accepted.available_at);
    let lease_token = uuid::Uuid::new_v4();
    let expiry = claimed_at + chrono::Duration::minutes(5);
    let claimed = store
        .claim_turn_run(lease_token, claimed_at, expiry)
        .await
        .unwrap()
        .turn
        .expect("turn should be claimable");
    assert_eq!(claimed.id, turn_id);
    let call = ToolCallRecord {
        id: CallId::new(),
        chat_id: chat.id,
        turn_id,
        provider_id: "approval-call".into(),
        name: tool_name.into(),
        arguments,
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
        created_at: claimed_at,
        resolved_at: None,
    };
    assert!(matches!(
        store.accept_tool_call(&call).await.unwrap(),
        AcceptToolCallOutcome::Accepted(_)
    ));
    let request = ApprovalRequest {
        auto_judge: false,
        call_id: call.id,
        chat_id: chat.id,
        turn_id,
        tool_name: call.name.clone(),
        class: ApprovalClass::Sensitive,
        kind: crate::ToolApprovalKind::for_tool_name(&call.name),
        preview: None,
    };
    (turn_id, lease_token, call, request)
}

#[tokio::test]
async fn workspace_approval_folds_storage_but_recovers_class_and_kind() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (_turn_id, _lease_token, call, mut request) = claimed_sensitive_call_with(
        &store,
        &chat,
        "write_file",
        serde_json::json!({"path": "notes.md", "content": "x"}),
    )
    .await;
    request.class = ApprovalClass::Workspace;
    request.kind = crate::ToolApprovalKind::WorkspaceMayModifyFiles;
    store
        .request_tool_call_approval(&request, Utc::now())
        .await
        .unwrap();

    // The card is one approval row whose id is the call id, carrying the
    // engine's own request; the read model recovers the class from the
    // kind, so a workspace card parked across a restart stays approvable.
    let row = entities::code_approval::Entity::find_by_id(call.id.0)
        .one(&store.conn)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.session_id, chat.id.0);
    assert_eq!(
        crate::InternalToolApprovalRequest::from_raw(&row.harness_raw)
            .unwrap()
            .kind,
        crate::ToolApprovalKind::WorkspaceMayModifyFiles
    );

    let approval = store
        .get_tool_call_approval(call.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(approval.class, ApprovalClass::Workspace);
    assert_eq!(
        approval.kind,
        crate::ToolApprovalKind::WorkspaceMayModifyFiles
    );
    assert!(approval.kind.is_approvable());
    // Grantable about a place, and nothing wider — the whole-tool rung is the
    // chat's Auto mode, not a standing grant.
    assert!(approval.kind.grantable_at(&crate::GrantScope::PathSubtree {
        prefix: "notes.md".into()
    }));
    assert!(!approval.kind.grantable_at(&crate::GrantScope::WholeTool));
}

#[tokio::test]
async fn recovered_exec_approval_still_names_the_command_it_will_run() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (_turn_id, lease_token, _call, request) = claimed_sensitive_call_with(
        &store,
        &chat,
        "exec",
        serde_json::json!({ "command": "cargo", "args": ["build"], "cwd": "checkout" }),
    )
    .await;

    store
        .request_tool_call_approval_and_append_event(
            &request,
            lease_token,
            1,
            DateTime::<Utc>::from_timestamp(1, 0).unwrap(),
        )
        .await
        .unwrap();

    // The preview is rebuilt from the arguments the call is parked on, so a
    // card recovered after a restart describes the command that will actually
    // run rather than whatever was in flight when the process died.
    let recovered = store
        .get_tool_call_approval(request.call_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        recovered.preview,
        Some(crate::ToolActionPreview::Exec {
            command: "cargo".into(),
            args: vec!["build".into()],
            cwd: "checkout".into(),
            files: Vec::new(),
            summary: None,
        })
    );
}

#[tokio::test]
async fn external_mcp_approval_roundtrips_as_one_shot_consent() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (_turn_id, lease_token, _call, request) =
        claimed_sensitive_call_named(&store, &chat, "mcp__documents__search").await;

    let registered = store
        .request_tool_call_approval_and_append_event(
            &request,
            lease_token,
            1,
            DateTime::<Utc>::from_timestamp(1, 0).unwrap(),
        )
        .await
        .unwrap();
    let approval = match registered.outcome {
        RequestToolApprovalOutcome::Requested(approval) => approval,
        outcome => panic!("unexpected approval request outcome: {outcome:?}"),
    };
    assert_eq!(
        approval.kind,
        crate::ToolApprovalKind::ExternalMcpMayCallServer
    );
    assert!(approval.kind.is_approvable());
    assert!(!approval.kind.is_standing_grantable());

    let recovered = store
        .get_tool_call_approval(request.call_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(recovered.kind, approval.kind);
}

#[tokio::test]
async fn approval_registration_journals_once_and_decision_is_exact() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (turn_id, lease_token, _call, request) = claimed_sensitive_call(&store, &chat).await;

    // Deliberately stale caller time: the claimed operation must use the DB
    // statement clock for both request ordering and lease freshness.
    let stale_clock = DateTime::<Utc>::from_timestamp(1, 0).unwrap();
    let first = store
        .request_tool_call_approval_and_append_event(&request, lease_token, 1, stale_clock)
        .await
        .unwrap();
    let requested = match first.outcome {
        RequestToolApprovalOutcome::Requested(approval) => approval,
        outcome => panic!("unexpected approval request outcome: {outcome:?}"),
    };
    assert!(requested.requested_at > stale_clock);
    let first_event = first.required_event.expect("request event must commit");
    assert_eq!(first_event.seq, 1);
    assert!(matches!(
        first_event.event,
        AgentEvent::ApprovalRequired { call_id, .. } if call_id == request.call_id
    ));

    let retry = store
        .request_tool_call_approval_and_append_event(&request, lease_token, 1, Utc::now())
        .await
        .unwrap();
    assert!(matches!(
        retry.outcome,
        RequestToolApprovalOutcome::Existing(ref approval) if approval == &requested
    ));
    assert_eq!(
        retry.required_event.as_ref().map(|event| event.seq),
        Some(1)
    );
    assert_eq!(store.list_events(chat.id, 0).await.unwrap().len(), 1);

    // A later attempt recovers the pending request without appending a second
    // ApprovalRequired event under its new claim identity.
    let failure_at = super::ops::agent_run::database_now(&store.conn)
        .await
        .unwrap()
        .max(
            store
                .get_turn_run(turn_id)
                .await
                .unwrap()
                .unwrap()
                .updated_at,
        );
    let retry_at = failure_at + chrono::Duration::seconds(1);
    assert!(store
        .record_turn_run_failure(
            turn_id,
            lease_token,
            failure_at,
            TurnFailureRetry::RetryAt(retry_at),
            0,
            crate::provider::Usage::default(),
            "worker_restarted",
            None,
        )
        .await
        .unwrap()
        .is_some());
    let after_lease_release = store
        .request_tool_call_approval_and_append_event(&request, lease_token, 1, Utc::now())
        .await
        .unwrap();
    assert!(matches!(
        after_lease_release.outcome,
        RequestToolApprovalOutcome::Existing(ref approval) if approval == &requested
    ));
    assert_eq!(
        after_lease_release
            .required_event
            .as_ref()
            .map(|event| event.seq),
        Some(1)
    );
    let resumed_lease = uuid::Uuid::new_v4();
    let resumed_claim = store
        .claim_turn_run(
            resumed_lease,
            retry_at,
            retry_at + chrono::Duration::minutes(5),
        )
        .await
        .unwrap();
    assert!(
        resumed_claim.turn.is_some(),
        "retry should be claimable: {resumed_claim:?}"
    );
    let resumed = store
        .request_tool_call_approval_and_append_event(&request, resumed_lease, 1, Utc::now())
        .await
        .unwrap();
    assert!(matches!(
        resumed.outcome,
        RequestToolApprovalOutcome::Existing(ref approval) if approval == &requested
    ));
    assert!(resumed.required_event.is_none());
    assert_eq!(store.list_events(chat.id, 0).await.unwrap().len(), 1);

    let decided = store
        .decide_tool_call_approval(
            chat.id,
            request.call_id,
            &ApprovalDecision::Approve,
            stale_clock,
        )
        .await
        .unwrap();
    let approved = match decided {
        DecideToolApprovalOutcome::Decided { approval, .. } => approval,
        outcome => panic!("unexpected approval decision outcome: {outcome:?}"),
    };
    assert_eq!(approved.status, ToolApprovalStatus::Approved);
    assert!(approved.decided_at.is_some_and(|decided_at| {
        decided_at >= requested.requested_at && decided_at > stale_clock
    }));
    let terminal_lost_ack = store
        .request_tool_call_approval_and_append_event(&request, lease_token, 1, Utc::now())
        .await
        .unwrap();
    assert!(matches!(
        terminal_lost_ack.outcome,
        RequestToolApprovalOutcome::Existing(ref existing) if existing == &approved
    ));
    assert_eq!(
        terminal_lost_ack
            .required_event
            .as_ref()
            .map(|event| event.seq),
        Some(1)
    );
    assert!(matches!(
        store
            .decide_tool_call_approval(
                chat.id,
                request.call_id,
                &ApprovalDecision::Approve,
                Utc::now(),
            )
            .await
            .unwrap(),
        DecideToolApprovalOutcome::Existing(existing) if existing == approved
    ));
    assert_eq!(
        store
            .decide_tool_call_approval(
                chat.id,
                request.call_id,
                &ApprovalDecision::Reject {
                    reason: "changed".into(),
                },
                Utc::now(),
            )
            .await
            .unwrap(),
        DecideToolApprovalOutcome::DecisionConflict
    );

    let terminal_retry = store
        .request_tool_call_approval_and_append_event(&request, resumed_lease, 1, Utc::now())
        .await
        .unwrap();
    assert!(matches!(
        terminal_retry.outcome,
        RequestToolApprovalOutcome::Existing(existing) if existing == approved
    ));
    assert!(terminal_retry.required_event.is_none());
    // One row for the request, one for the decision, whichever surface made
    // it; a retry of either adds nothing.
    let journal = store.list_events(chat.id, 0).await.unwrap();
    assert_eq!(journal.len(), 2);
    assert!(matches!(
        journal[1].event,
        AgentEvent::ApprovalDecided { call_id, approved: true } if call_id == request.call_id
    ));
}

#[tokio::test]
async fn delete_chat_erases_a_terminal_approval_receipt_before_its_event() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (turn_id, lease_token, _call, request) = claimed_sensitive_call(&store, &chat).await;

    let registered = store
        .request_tool_call_approval_and_append_event(&request, lease_token, 1, Utc::now())
        .await
        .unwrap();
    assert!(registered.required_event.is_some());

    let cancel_at = super::ops::agent_run::database_now(&store.conn)
        .await
        .unwrap()
        .max(
            store
                .get_turn_run(turn_id)
                .await
                .unwrap()
                .unwrap()
                .updated_at,
        );
    assert!(store
        .request_turn_cancellation(turn_id, cancel_at)
        .await
        .unwrap()
        .is_some());
    let finish_at = super::ops::agent_run::database_now(&store.conn)
        .await
        .unwrap()
        .max(
            store
                .get_turn_run(turn_id)
                .await
                .unwrap()
                .unwrap()
                .updated_at,
        );
    assert!(store
        .finish_turn_cancellation(turn_id, lease_token, finish_at)
        .await
        .unwrap()
        .is_some());

    assert!(matches!(
        store.delete_chat(chat.id).await.unwrap(),
        DeleteChatOutcome::Deleted { .. }
    ));
    assert!(store.get_chat(chat.id).await.unwrap().is_none());
    assert!(store.list_events(chat.id, 0).await.unwrap().is_empty());
}

#[tokio::test]
async fn cancellation_closes_pending_approval_and_tool_atomically() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (turn_id, lease_token, call, request) = claimed_sensitive_call(&store, &chat).await;
    let pending = store
        .request_tool_call_approval_and_append_event(&request, lease_token, 1, Utc::now())
        .await
        .unwrap();
    assert!(matches!(
        pending.outcome,
        RequestToolApprovalOutcome::Requested(_)
    ));
    assert_eq!(
        store
            .list_pending_tool_call_approvals(chat.id, 100)
            .await
            .unwrap()
            .len(),
        1
    );

    let cancel_at = super::ops::agent_run::database_now(&store.conn)
        .await
        .unwrap()
        .max(
            store
                .get_turn_run(turn_id)
                .await
                .unwrap()
                .unwrap()
                .updated_at,
        );
    assert!(store
        .request_turn_cancellation(turn_id, cancel_at)
        .await
        .unwrap()
        .is_some());
    assert!(store
        .list_pending_tool_call_approvals(chat.id, 100)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        store
            .decide_tool_call_approval(chat.id, call.id, &ApprovalDecision::Approve, Utc::now(),)
            .await
            .unwrap(),
        DecideToolApprovalOutcome::DecisionConflict
    );
    let finish_at = super::ops::agent_run::database_now(&store.conn)
        .await
        .unwrap()
        .max(
            store
                .get_turn_run(turn_id)
                .await
                .unwrap()
                .unwrap()
                .updated_at,
        );
    assert!(store
        .finish_turn_cancellation(turn_id, lease_token, finish_at)
        .await
        .unwrap()
        .is_some());

    assert!(store
        .list_pending_tool_call_approvals(chat.id, 100)
        .await
        .unwrap()
        .is_empty());
    let approval = store
        .get_tool_call_approval(call.id)
        .await
        .unwrap()
        .expect("terminal approval receipt must remain");
    assert_eq!(approval.status, ToolApprovalStatus::Rejected);
    assert_eq!(
        approval.reason.as_deref(),
        Some("turn cancellation revoked approval")
    );
    let stored_call = store
        .list_tool_calls(chat.id)
        .await
        .unwrap()
        .into_iter()
        .find(|stored| stored.id == call.id)
        .unwrap();
    assert_eq!(stored_call.status, ToolCallStatus::Cancelled);
    assert_eq!(
        store
            .decide_tool_call_approval(chat.id, call.id, &ApprovalDecision::Approve, Utc::now(),)
            .await
            .unwrap(),
        DecideToolApprovalOutcome::DecisionConflict
    );
}

#[tokio::test]
async fn retry_wait_cancellation_terminalizes_pending_call_and_approval() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (turn_id, lease_token, call, request) = claimed_sensitive_call(&store, &chat).await;
    store
        .request_tool_call_approval_and_append_event(&request, lease_token, 1, Utc::now())
        .await
        .unwrap();

    let failure_at = super::ops::agent_run::database_now(&store.conn)
        .await
        .unwrap()
        .max(
            store
                .get_turn_run(turn_id)
                .await
                .unwrap()
                .unwrap()
                .updated_at,
        );
    assert!(store
        .record_turn_run_failure(
            turn_id,
            lease_token,
            failure_at,
            TurnFailureRetry::RetryAt(failure_at + chrono::Duration::minutes(1)),
            0,
            crate::provider::Usage::default(),
            "retry_later",
            None,
        )
        .await
        .unwrap()
        .is_some());
    assert_eq!(
        store.get_turn_run(turn_id).await.unwrap().unwrap().status,
        TurnRunStatus::RetryWait
    );

    let cancel_at = super::ops::agent_run::database_now(&store.conn)
        .await
        .unwrap()
        .max(
            store
                .get_turn_run(turn_id)
                .await
                .unwrap()
                .unwrap()
                .updated_at,
        );
    assert!(matches!(
        store
            .request_turn_cancellation(turn_id, cancel_at)
            .await
            .unwrap(),
        Some(RequestTurnCancellationOutcome::Cancelled(turn))
            if turn.status == TurnRunStatus::Cancelled
    ));
    assert_eq!(
        store
            .list_tool_calls(chat.id)
            .await
            .unwrap()
            .into_iter()
            .find(|stored| stored.id == call.id)
            .unwrap()
            .status,
        ToolCallStatus::Cancelled
    );
    assert_eq!(
        store
            .get_tool_call_approval(call.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        ToolApprovalStatus::Rejected
    );
    assert!(store
        .list_pending_tool_call_approvals(chat.id, 100)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        store
            .decide_tool_call_approval(chat.id, call.id, &ApprovalDecision::Approve, Utc::now())
            .await
            .unwrap(),
        DecideToolApprovalOutcome::DecisionConflict
    );
}

#[tokio::test]
async fn cancellation_and_approval_decision_serialize_without_pending_state() {
    let (_dir, store) = temp_store().await;
    let store = std::sync::Arc::new(store);
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (turn_id, _lease_token, call, request) = claimed_sensitive_call(&store, &chat).await;
    store
        .request_tool_call_approval(&request, Utc::now())
        .await
        .unwrap();
    let cancel_at = super::ops::agent_run::database_now(&store.conn)
        .await
        .unwrap()
        .max(
            store
                .get_turn_run(turn_id)
                .await
                .unwrap()
                .unwrap()
                .updated_at,
        );
    let chat_id = chat.id;
    let call_id = call.id;
    let cancelling = store.clone();
    let deciding = store.clone();
    let (cancelled, decided) = tokio::join!(
        async move {
            cancelling
                .request_turn_cancellation(turn_id, cancel_at)
                .await
        },
        async move {
            deciding
                .decide_tool_call_approval(chat_id, call_id, &ApprovalDecision::Approve, Utc::now())
                .await
        }
    );
    assert!(cancelled.unwrap().is_some());
    assert!(matches!(
        decided.unwrap(),
        DecideToolApprovalOutcome::Decided { .. } | DecideToolApprovalOutcome::DecisionConflict
    ));
    assert!(store
        .list_pending_tool_call_approvals(chat_id, 100)
        .await
        .unwrap()
        .is_empty());
    assert_ne!(
        store
            .get_tool_call_approval(call_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        ToolApprovalStatus::Pending
    );
}

#[tokio::test]
async fn failed_tool_resolution_and_approval_decision_serialize_to_one_terminal_receipt() {
    let (_dir, store) = temp_store().await;
    let store = std::sync::Arc::new(store);
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (_turn_id, _lease_token, call, request) = claimed_sensitive_call(&store, &chat).await;
    store
        .request_tool_call_approval(&request, Utc::now())
        .await
        .unwrap();

    let chat_id = chat.id;
    let call_id = call.id;
    let resolving = store.clone();
    let deciding = store.clone();
    let resolution = ToolCallResolution::Failed {
        result: "tool implementation is unavailable".into(),
        error_code: "tool_error".into(),
        error_detail: None,
    };
    let (resolved, decided) = tokio::join!(
        async move {
            resolving
                .resolve_server_tool_call(call_id, &resolution, Utc::now())
                .await
        },
        async move {
            deciding
                .decide_tool_call_approval(chat_id, call_id, &ApprovalDecision::Approve, Utc::now())
                .await
        }
    );
    assert!(matches!(
        resolved.unwrap(),
        ResolveToolCallOutcome::Resolved | ResolveToolCallOutcome::Existing
    ));
    assert!(matches!(
        decided.unwrap(),
        DecideToolApprovalOutcome::Decided { .. } | DecideToolApprovalOutcome::DecisionConflict
    ));
    let approval = store
        .get_tool_call_approval(call_id)
        .await
        .unwrap()
        .expect("approval receipt must remain");
    assert_ne!(approval.status, ToolApprovalStatus::Pending);
    assert!(store
        .list_pending_tool_call_approvals(chat_id, 100)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        store
            .list_tool_calls(chat_id)
            .await
            .unwrap()
            .into_iter()
            .find(|stored| stored.id == call_id)
            .unwrap()
            .status,
        ToolCallStatus::Failed
    );
}

#[tokio::test]
async fn approval_reject_reason_rejects_controls_before_commit() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (_turn_id, lease_token, call, request) = claimed_sensitive_call(&store, &chat).await;
    store
        .request_tool_call_approval_and_append_event(&request, lease_token, 1, Utc::now())
        .await
        .unwrap();
    assert!(store
        .decide_tool_call_approval(
            chat.id,
            call.id,
            &ApprovalDecision::Reject {
                reason: "bad\0reason".into(),
            },
            Utc::now(),
        )
        .await
        .is_err());
    assert_eq!(
        store
            .get_tool_call_approval(call.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        ToolApprovalStatus::Pending
    );
}
