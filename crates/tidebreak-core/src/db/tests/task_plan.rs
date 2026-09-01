use super::*;

/// Accepting a plan is the whole hand-off in one transaction: the pending card
/// survives a restart, the decision completes the call exactly once, and the
/// chat leaves plan mode so the resumed turn re-freezes with execution tools.
#[tokio::test]
async fn accepted_plan_resumes_the_turn_and_leaves_plan_mode() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("plans.db").display()
    );
    let store = DbStore::connect(&url).await.unwrap();
    let chat = Chat {
        permission_mode: Some(crate::PermissionMode::Plan),
        ..sample_chat()
    };
    store.create_chat(&chat).await.unwrap();
    let (turn_id, request, parked_at) = park_test_plan(&store, chat.id).await;
    drop(store);

    let restarted = DbStore::connect(&url).await.unwrap();
    let pending = restarted
        .list_pending_plan_approvals(chat.id)
        .await
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].call_id, request.id);
    assert_eq!(pending[0].turn_id, turn_id);
    assert_eq!(pending[0].title, "Add health checks");
    assert!(pending[0].plan.contains("/healthz"));

    let decide_request = crate::DecidePlanRequest {
        chat_id: chat.id,
        call_id: request.id,
        decision: crate::PlanDecision {
            decision: crate::PlanDecisionChoice::Accept,
            feedback: None,
            permission_mode: None,
        },
    };
    let decided_at = parked_at + chrono::Duration::seconds(1);
    let outcome = restarted
        .decide_plan(&decide_request, decided_at)
        .await
        .unwrap();
    let crate::storage::DecidePlanOutcome::Decided {
        turn,
        completion_event,
    } = outcome
    else {
        panic!("unexpected plan decision: {outcome:?}");
    };
    assert_eq!(turn.id, turn_id);
    assert_eq!(turn.status, TurnRunStatus::Resuming);
    // The call resolves outside the agent loop, so this event is the only
    // thing that tells a connected renderer the card settled — and it carries
    // the recap the transcript shows in the pending card's place.
    let crate::AgentEvent::ToolCallCompleted {
        call_id,
        result: Some(preview),
        ..
    } = &completion_event.event
    else {
        panic!("unexpected completion: {:?}", completion_event.event);
    };
    assert_eq!(*call_id, request.id);
    assert!(matches!(
        preview,
        crate::ToolResultPreview::PlanDecision {
            title,
            plan,
            accepted: true,
            feedback: None,
        } if title == "Add health checks" && plan.contains("/healthz")
    ));
    assert_eq!(
        restarted
            .get_chat(chat.id)
            .await
            .unwrap()
            .unwrap()
            .permission_mode,
        Some(crate::PermissionMode::Auto),
        "accepting must move the chat out of plan mode"
    );
    assert!(restarted
        .list_pending_plan_approvals(chat.id)
        .await
        .unwrap()
        .is_empty());
    let calls = restarted.list_tool_calls(chat.id).await.unwrap();
    let call = calls.iter().find(|call| call.id == request.id).unwrap();
    let result: serde_json::Value = serde_json::from_str(call.result.as_deref().unwrap()).unwrap();
    assert_eq!(result["decision"], "accepted");
    assert!(result["note"].as_str().unwrap().contains("left plan mode"));
    // Rehydration reads the stored column, not the event, so a reload has to
    // find the same recap there.
    assert_eq!(call.result_preview.as_ref(), Some(preview));

    // An exact retry recovers; a different decision conflicts.
    assert!(matches!(
        restarted.decide_plan(&decide_request, decided_at).await.unwrap(),
        crate::storage::DecidePlanOutcome::Existing(turn) if turn.id == turn_id
    ));
    let contradictory = crate::DecidePlanRequest {
        decision: crate::PlanDecision {
            decision: crate::PlanDecisionChoice::Reject,
            feedback: Some("Different call.".into()),
            permission_mode: None,
        },
        ..decide_request
    };
    assert!(matches!(
        restarted
            .decide_plan(&contradictory, decided_at)
            .await
            .unwrap(),
        crate::storage::DecidePlanOutcome::DecisionConflict
    ));
}

/// Rejecting keeps the chat in plan mode and hands the feedback to the model.
#[tokio::test]
async fn rejected_plan_keeps_plan_mode_and_carries_feedback() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("plan-reject.db").display()
    );
    let store = DbStore::connect(&url).await.unwrap();
    let chat = Chat {
        permission_mode: Some(crate::PermissionMode::Plan),
        ..sample_chat()
    };
    store.create_chat(&chat).await.unwrap();
    let (turn_id, request, parked_at) = park_test_plan(&store, chat.id).await;

    let decided_at = parked_at + chrono::Duration::seconds(1);
    assert!(matches!(
        store
            .decide_plan(
                &crate::DecidePlanRequest {
                    chat_id: chat.id,
                    call_id: request.id,
                    decision: crate::PlanDecision {
                        decision: crate::PlanDecisionChoice::Reject,
                        feedback: Some("Split step 2 into its own slice.".into()),
                        permission_mode: None,
                    },
                },
                decided_at,
            )
            .await
            .unwrap(),
        crate::storage::DecidePlanOutcome::Decided { turn, .. }
            if turn.id == turn_id && turn.status == TurnRunStatus::Resuming
    ));
    assert_eq!(
        store
            .get_chat(chat.id)
            .await
            .unwrap()
            .unwrap()
            .permission_mode,
        Some(crate::PermissionMode::Plan),
        "rejecting must keep the chat in plan mode"
    );
    let calls = store.list_tool_calls(chat.id).await.unwrap();
    let call = calls.iter().find(|call| call.id == request.id).unwrap();
    let result: serde_json::Value = serde_json::from_str(call.result.as_deref().unwrap()).unwrap();
    assert_eq!(result["decision"], "rejected");
    assert_eq!(result["feedback"], "Split step 2 into its own slice.");
    assert!(matches!(
        call.result_preview.as_ref(),
        Some(crate::ToolResultPreview::PlanDecision {
            accepted: false,
            feedback: Some(feedback),
            ..
        }) if feedback == "Split step 2 into its own slice."
    ));
}

/// A plan write is a live-turn journal write, so it takes the same lease fence
/// as every other one: a stalled attempt that lost its turn must not overwrite
/// the plan the current attempt is being judged by, and one call must record
/// its plan once however many times the loop replays it.
#[tokio::test]
async fn task_plan_writes_are_fenced_on_the_turn_lease_and_recorded_once() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    let accepted = match store
        .accept_turn(turn_id, chat.id, "gpt-5", "plan the work")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected turn acceptance: {outcome:?}"),
    };
    let claim_at = accepted.available_at;
    let stale_lease = uuid::Uuid::new_v4();
    store
        .claim_turn_run(
            stale_lease,
            claim_at,
            claim_at + chrono::Duration::seconds(1),
        )
        .await
        .unwrap();
    let call = |provider_id: &str| ToolCallRecord {
        id: CallId::new(),
        chat_id: chat.id,
        turn_id,
        provider_id: provider_id.into(),
        name: crate::UPDATE_TASK_PLAN_TOOL.into(),
        arguments: serde_json::json!({"steps": []}),
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
        created_at: claim_at,
        resolved_at: None,
    };
    // Both calls are admitted while the first attempt still owns the turn.
    let first = call("tu_plan_1");
    let stalled = call("tu_plan_2");
    for record in [&first, &stalled] {
        assert!(matches!(
            store
                .accept_claimed_tool_call(record, stale_lease, claim_at)
                .await
                .unwrap(),
            AcceptClaimedToolCallOutcome::Accepted(_)
        ));
    }
    let step = |content: &str| crate::TaskPlanStep {
        content: content.into(),
        status: crate::TaskPlanStepStatus::InProgress,
    };

    let recorded = store
        .update_task_plan(chat.id, first.id, &[step("draft the change")], Utc::now())
        .await
        .unwrap()
        .expect("the owning attempt records its plan");
    assert_eq!(recorded.turn_id, turn_id);
    // The row and the projection agree on when it committed; the row's value
    // is clamped to what the database can hold and the caller sees that one.
    assert_eq!(
        store.get_task_plan(chat.id).await.unwrap(),
        Some(recorded.clone())
    );
    let hints = |events: Vec<crate::SequencedEvent>| {
        events
            .into_iter()
            .filter(|event| matches!(event.event, AgentEvent::TaskPlanUpdated { .. }))
            .count()
    };
    assert_eq!(hints(store.list_events(chat.id, 0).await.unwrap()), 1);

    // The loop re-executing a row it already admitted must not journal a
    // second hint for the same call.
    assert_eq!(
        store
            .update_task_plan(chat.id, first.id, &[step("draft the change")], Utc::now())
            .await
            .unwrap(),
        Some(recorded.clone())
    );
    assert_eq!(hints(store.list_events(chat.id, 0).await.unwrap()), 1);

    // The turn is reclaimed by a later attempt. The first attempt is still
    // running somewhere with an admitted call in hand.
    let retry_at = claim_at + chrono::Duration::seconds(2);
    store
        .claim_turn_run(
            uuid::Uuid::new_v4(),
            retry_at,
            retry_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .update_task_plan(chat.id, stalled.id, &[step("stale intent")], Utc::now())
            .await
            .unwrap(),
        None,
        "an attempt that lost its lease must not replace the live plan"
    );
    assert_eq!(store.get_task_plan(chat.id).await.unwrap(), Some(recorded));
    assert_eq!(hints(store.list_events(chat.id, 0).await.unwrap()), 1);
}
