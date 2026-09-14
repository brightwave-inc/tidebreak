//! Managed helper calls cross the real socket, durable approval, and result transport.
use super::tests::fixture;
use super::*;
use crate::code::remote::driver::HostToolExecutor;
use crate::code::runtime::ApprovalDecisionRequest;
use tidebreak_core::code::supervisor_tools::encode_result_frames;
use tidebreak_core::code::{ApprovalDecisionKind, SupervisorToolTurn};
use tidebreak_core::db::code::*;
use tidebreak_core::storage::Store;
use tidebreak_core::{
    Approval, ApprovalState, Event, HarnessKind, QueuedTurn, Session, Turn, TurnActor, TurnId,
    TurnStatus, UserQuestionAnswer,
};
#[cfg(unix)]
use tidebreak_supervised_agent::tool_bridge::{call, LocalToolBridge};

fn question_request(id: &str) -> SupervisorToolRequest {
    SupervisorToolRequest {
        cancelled: false,
        request_id: id.into(),
        tool: "ask_user_questions".into(),
        turn: None,
        arguments: serde_json::json!({"questions":[{
            "id":"target","header":"Target","question":"Which target?",
            "options":[{"id":"test","label":"Test","description":"Use the test target"},
                {"id":"live","label":"Live","description":"Use the live target"}],
            "question_type":"single_select","allow_free_form":true
        }]}),
    }
}

fn answer(options: &[&str], custom: Option<&str>) -> ApprovalDecisionRequest {
    ApprovalDecisionRequest::Answers {
        answers: vec![UserQuestionAnswer {
            question_id: "target".into(),
            selected_option_ids: options.iter().map(|s| (*s).to_owned()).collect(),
            custom_answer: custom.map(str::to_owned),
        }],
    }
}

async fn active_turn(
    runtime: &CodeRuntime,
    session: &Session,
    incarnation: CodeIncarnationId,
) -> (Turn, SupervisorToolTurn) {
    let turn = Turn {
        id: TurnId::new(),
        session_id: session.id,
        ordinal: 1,
        status: TurnStatus::Running,
        model: None,
        fast_mode: false,
        actor: None,
        user_input: "Run the fixture".into(),
        user_input_blob_id: None,
        attachments: vec![],
        checkpoint_ref: None,
        diffstat: None,
        usage: None,
        narrative: None,
        rewrite: None,
        started_at: chrono::Utc::now(),
        ended_at: None,
        park_ref: None,
        park_wait: None,
    };
    insert_turn(&runtime.db, &session.owner, &turn)
        .await
        .unwrap();
    let identity = SupervisorToolTurn {
        native_turn: 1,
        runtime_id: uuid::Uuid::new_v4(),
    };
    runtime.db.set_setting(&format!("code.incarnations.{incarnation}.steering_protocol"), &serde_json::json!({
        "version":1,"runtime_id":identity.runtime_id.to_string(),"sandbox_id":"fixture-sandbox"
    })).await.unwrap();
    (turn, identity)
}

async fn only_approval(runtime: &CodeRuntime, session: &Session) -> Approval {
    let approvals = list_approvals(&runtime.db, &session.owner, None, Some(session.id))
        .await
        .unwrap();
    assert_eq!(approvals.len(), 1);
    approvals.into_iter().next().unwrap()
}

#[cfg(unix)]
async fn admitted(bridge: &mut LocalToolBridge) -> SupervisorToolRequest {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(request) = bridge.drain_requests().into_iter().next() {
                return request;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("helper request reaches supervisor")
}

#[cfg(unix)]
#[tokio::test]
async fn managed_questions_wait_for_an_explicit_answer() {
    for harness in [HarnessKind::ClaudeCode, HarnessKind::Codex] {
        for (question_type, choices, custom) in [
            ("single_select", vec!["test"], None),
            ("multi_select", vec!["test", "live"], None),
            ("single_select", vec![], Some("Another target")),
        ] {
            let (dir, runtime, mut session, incarnation, calls) =
                fixture(true, PermissionMode::Allow).await;
            session.harness_kind = harness;
            save_session(&runtime.db, &session).await.unwrap();
            let (_, identity) = active_turn(&runtime, &session, incarnation).await;
            let mut bridge = LocalToolBridge::start(dir.path()).unwrap();
            bridge.begin_turn(identity.clone());
            let executor = SandboxToolExecutor::new(Arc::downgrade(&runtime));
            let mut request = question_request("human-1");
            request.arguments["questions"][0]["question_type"] = serde_json::json!(question_type);
            let socket = bridge.socket_path();
            let helper = tokio::spawn(async move { call(&socket, &request).await });
            let request = admitted(&mut bridge).await;
            assert_eq!(request.turn, Some(identity));
            executor
                .enqueue(&session.owner, session.id, incarnation, &request)
                .await
                .unwrap();
            executor
                .enqueue(&session.owner, session.id, incarnation, &request)
                .await
                .unwrap();
            let approval = only_approval(&runtime, &session).await;
            assert_eq!(approval.state, ApprovalState::Pending);
            assert!(approval.auto_judge_status.is_none());
            assert!(!helper.is_finished());
            assert!(executor
                .service(&session.owner, session.id, incarnation)
                .await
                .unwrap()
                .is_empty());
            assert_eq!(executor.slots.available_permits(), MAX_WORKERS);
            assert_eq!(calls.count.load(Ordering::SeqCst), 0);
            assert!(list_native_tool_requests(
                &runtime.db,
                &session.owner,
                session.id,
                incarnation
            )
            .await
            .unwrap()
            .is_empty());
            assert!(matches!(
                runtime
                    .get_session(&session.owner, session.id)
                    .await
                    .unwrap()
                    .attention
                    .state,
                tidebreak_core::AttentionState::NeedsYou { .. }
            ));
            let actor = TurnActor {
                display: Some("Alex".into()),
                channel_kind: Some("slack".into()),
                external_identity: Some("U-ALEX".into()),
                ..Default::default()
            };
            runtime
                .decide_approval(
                    &session.owner,
                    approval.id,
                    answer(&choices, custom),
                    Some(actor.clone()),
                )
                .await
                .unwrap();
            let results = executor
                .service(&session.owner, session.id, incarnation)
                .await
                .unwrap();
            assert_eq!(results.len(), 1);
            for frame in encode_result_frames(&results[0]).unwrap() {
                bridge.receive_frame(&frame).unwrap();
            }
            let output = helper.await.unwrap().unwrap();
            assert_eq!(output["output"]["data"]["decision"], "answered");
            assert_eq!(
                output["output"]["data"]["answers"][0]["custom_answer"].as_str(),
                custom
            );
            assert_eq!(
                only_approval(&runtime, &session).await.actor,
                Some(actor.clone())
            );
            let events = list_events(&runtime.db, &session.owner, session.id, 0, 50)
                .await
                .unwrap();
            assert_eq!(
                events
                    .events
                    .iter()
                    .filter(|e| matches!(e.event, Event::ApprovalRequested { .. }))
                    .count(),
                1
            );
            assert_eq!(events.events.iter().filter(|e| matches!(&e.event, Event::ApprovalResolved { actor: Some(who), decision: ApprovalDecisionKind::Answered { .. }, .. } if who == &actor)).count(), 1);
            executor
                .mark_delivered(&session.owner, session.id, incarnation, "human-1")
                .await
                .unwrap();
            assert!(executor
                .service(&session.owner, session.id, incarnation)
                .await
                .unwrap()
                .is_empty());
        }
    }
}

#[tokio::test]
async fn managed_decisions_reject_invalid_answers_and_changed_replays() {
    let (_dir, runtime, session, incarnation, _) = fixture(true, PermissionMode::Allow).await;
    let (_, identity) = active_turn(&runtime, &session, incarnation).await;
    let executor = SandboxToolExecutor::new(Arc::downgrade(&runtime));
    let mut request = question_request("human-1");
    request.turn = Some(identity);
    request.arguments["questions"][0]["allow_free_form"] = serde_json::json!(false);
    executor
        .enqueue(&session.owner, session.id, incarnation, &request)
        .await
        .unwrap();
    let approval = only_approval(&runtime, &session).await;
    for invalid in [
        ApprovalDecisionRequest::Approve,
        ApprovalDecisionRequest::Answers { answers: vec![] },
        answer(&["missing"], None),
        answer(&["test", "live"], None),
        answer(&["test", "test"], None),
        answer(&[], Some("custom")),
        ApprovalDecisionRequest::PlanDecision {
            approve: true,
            feedback: None,
        },
    ] {
        assert!(runtime
            .decide_approval(&session.owner, approval.id, invalid, None)
            .await
            .is_err());
        assert_eq!(
            only_approval(&runtime, &session).await.state,
            ApprovalState::Pending
        );
    }
    let foreign = OwnerId::new("foreign-owner").unwrap();
    assert!(runtime
        .decide_approval(&foreign, approval.id, answer(&["test"], None), None)
        .await
        .is_err());
    let mut changed = request.clone();
    changed.arguments["questions"][0]["question"] = serde_json::json!("Different target?");
    assert!(executor
        .enqueue(&session.owner, session.id, incarnation, &changed)
        .await
        .is_err());
    let mut ordinary = request.clone();
    ordinary.tool = "code_wait".into();
    ordinary.arguments = serde_json::json!({});
    ordinary.turn = None;
    assert!(executor
        .enqueue(&session.owner, session.id, incarnation, &ordinary)
        .await
        .is_err());
    assert_eq!(
        only_approval(&runtime, &session).await.state,
        ApprovalState::Pending
    );
}

#[tokio::test]
async fn managed_decisions_survive_restart_without_promoting_a_held_queue() {
    let (dir, runtime, session, incarnation, _) = fixture(true, PermissionMode::Allow).await;
    let (_, identity) = active_turn(&runtime, &session, incarnation).await;
    let executor = SandboxToolExecutor::new(Arc::downgrade(&runtime));
    let mut request = question_request("human-1");
    request.turn = Some(identity);
    executor
        .enqueue(&session.owner, session.id, incarnation, &request)
        .await
        .unwrap();
    let approval = only_approval(&runtime, &session).await;
    let now = chrono::Utc::now();
    let queued = enqueue_queued_turn(
        &runtime.db,
        &session.owner,
        &QueuedTurn {
            id: TurnId::new(),
            session_id: session.id,
            message: "Keep this follow-up held".into(),
            actor: None,
            attachments: vec![],
            position: 0,
            created_at: now,
            updated_at: now,
        },
    )
    .await
    .unwrap();
    set_queue_paused(&runtime.db, &session.owner, session.id, true)
        .await
        .unwrap();
    drop(executor);
    drop(runtime);
    let db = Arc::new(
        tidebreak_core::DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("native.db").display()
        ))
        .await
        .unwrap(),
    );
    let runtime = Arc::new(CodeRuntime::new(
        db,
        dir.path().into(),
        Some(dir.path().join("worktrees")),
        None,
        None,
        None,
        None,
        None,
    ));
    crate::code::approval_sweep::abandon_for_restart(
        &runtime.db,
        &runtime.bus,
        &session.owner,
        session.id,
        session.spawn_epoch,
    )
    .await;
    assert_eq!(
        only_approval(&runtime, &session).await.state,
        ApprovalState::Pending
    );
    let executor = SandboxToolExecutor::new(Arc::downgrade(&runtime));
    assert!(executor
        .service(&session.owner, session.id, incarnation)
        .await
        .unwrap()
        .is_empty());
    runtime
        .decide_approval(&session.owner, approval.id, answer(&["test"], None), None)
        .await
        .unwrap();
    let results = executor
        .service(&session.owner, session.id, incarnation)
        .await
        .unwrap();
    assert_eq!(results.len(), 1);
    drop(executor);
    let executor = SandboxToolExecutor::new(Arc::downgrade(&runtime));
    assert_eq!(
        executor
            .service(&session.owner, session.id, incarnation)
            .await
            .unwrap(),
        results
    );
    assert!(runtime
        .decide_approval(&session.owner, approval.id, answer(&["live"], None), None)
        .await
        .is_err());
    assert_eq!(
        list_queued_turns(&runtime.db, &session.owner, session.id)
            .await
            .unwrap(),
        vec![queued]
    );
    assert!(queue_paused(&runtime.db, &session.owner, session.id)
        .await
        .unwrap());
}

#[tokio::test]
async fn managed_decisions_abandon_stopped_turns_and_refuse_replaced_incarnations() {
    for reason in ["idle_timeout", "spend_ceiling", "cancelled"] {
        let (_dir, runtime, session, incarnation, _) = fixture(true, PermissionMode::Allow).await;
        let (_, identity) = active_turn(&runtime, &session, incarnation).await;
        let executor = SandboxToolExecutor::new(Arc::downgrade(&runtime));
        let mut request = question_request("human-1");
        request.turn = Some(identity);
        executor
            .enqueue(&session.owner, session.id, incarnation, &request)
            .await
            .unwrap();
        let approval = only_approval(&runtime, &session).await;
        stop_incarnation(&runtime.db, &session.owner, incarnation, Some(reason))
            .await
            .unwrap();
        let events = reconcile_managed_decisions(&runtime.db, &session.owner, session.id)
            .await
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(
            only_approval(&runtime, &session).await.state,
            ApprovalState::Abandoned
        );
        assert!(runtime
            .decide_approval(&session.owner, approval.id, answer(&["test"], None), None)
            .await
            .is_err());
        let tidebreak_core::IncarnationAdmission::Admitted(next) =
            create_incarnation_intent(&runtime.db, &session.owner, session.id, 1, 8)
                .await
                .unwrap()
        else {
            panic!("next incarnation")
        };
        activate_incarnation(&runtime.db, &session.owner, next.id, "replacement")
            .await
            .unwrap();
        assert!(executor
            .authorize_delivery(&session.owner, session.id, incarnation, "human-1")
            .await
            .is_err());
        assert!(executor
            .enqueue(&session.owner, session.id, next.id, &request)
            .await
            .is_err());
        assert!(
            managed_decision_results(&runtime.db, &session.owner, session.id, next.id)
                .await
                .unwrap()
                .is_empty()
        );
    }
}

#[tokio::test]
async fn managed_decisions_bind_the_exact_turn_and_supervisor() {
    let (_dir, runtime, session, incarnation, _) = fixture(true, PermissionMode::Allow).await;
    let (mut turn, identity) = active_turn(&runtime, &session, incarnation).await;
    let executor = SandboxToolExecutor::new(Arc::downgrade(&runtime));
    let mut request = question_request("human-1");
    request.turn = Some(identity.clone());
    executor
        .enqueue(&session.owner, session.id, incarnation, &request)
        .await
        .unwrap();
    let approval = only_approval(&runtime, &session).await;
    turn.status = TurnStatus::Completed;
    save_turn(&runtime.db, &session.owner, &turn).await.unwrap();
    assert!(runtime
        .decide_approval(&session.owner, approval.id, answer(&["test"], None), None)
        .await
        .is_err());
    assert_eq!(
        reconcile_managed_decisions(&runtime.db, &session.owner, session.id)
            .await
            .unwrap()
            .len(),
        1
    );
    turn.id = TurnId::new();
    turn.ordinal = 2;
    turn.status = TurnStatus::Running;
    insert_turn(&runtime.db, &session.owner, &turn)
        .await
        .unwrap();
    assert!(executor
        .enqueue(&session.owner, session.id, incarnation, &request)
        .await
        .is_err());
    request.request_id = "human-2".into();
    request.turn.as_mut().unwrap().native_turn = 2;
    executor
        .enqueue(&session.owner, session.id, incarnation, &request)
        .await
        .unwrap();
    runtime.db.set_setting(&format!("code.incarnations.{incarnation}.steering_protocol"), &serde_json::json!({"runtime_id":uuid::Uuid::new_v4().to_string(),"sandbox_id":"fixture-sandbox"})).await.unwrap();
    assert_eq!(
        reconcile_managed_decisions(&runtime.db, &session.owner, session.id)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(list_approvals(
        &runtime.db,
        &session.owner,
        Some(ApprovalState::Pending),
        Some(session.id)
    )
    .await
    .unwrap()
    .is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn managed_plan_acceptance_and_rejection_return_the_exact_decision_without_changing_allow() {
    for approve in [true, false] {
        let (dir, runtime, session, incarnation, _) = fixture(true, PermissionMode::Allow).await;
        let (_, identity) = active_turn(&runtime, &session, incarnation).await;
        let mut bridge = LocalToolBridge::start(dir.path()).unwrap();
        bridge.begin_turn(identity);
        let request = SupervisorToolRequest {
            cancelled: false,
            request_id: "plan-1".into(),
            tool: "request_plan_approval".into(),
            turn: None,
            arguments: serde_json::json!({"title":"Update the test target","plan":"Inspect the test target, apply the requested change, and verify the result before reporting completion."}),
        };
        let socket = bridge.socket_path();
        let helper = tokio::spawn(async move { call(&socket, &request).await });
        let request = admitted(&mut bridge).await;
        let executor = SandboxToolExecutor::new(Arc::downgrade(&runtime));
        executor
            .enqueue(&session.owner, session.id, incarnation, &request)
            .await
            .unwrap();
        let approval = only_approval(&runtime, &session).await;
        assert_eq!(
            approval.kind,
            tidebreak_core::ApprovalKind::Plan {
                proposed_mode: PermissionMode::Allow
            }
        );
        assert!(tidebreak_core::PlanProposalBody::from_raw(&approval.harness_raw).is_some());
        assert!(!helper.is_finished());
        runtime
            .decide_approval(
                &session.owner,
                approval.id,
                ApprovalDecisionRequest::PlanDecision {
                    approve,
                    feedback: Some("Keep it scoped".into()),
                },
                None,
            )
            .await
            .unwrap();
        let result = executor
            .service(&session.owner, session.id, incarnation)
            .await
            .unwrap()
            .remove(0);
        for frame in encode_result_frames(&result).unwrap() {
            bridge.receive_frame(&frame).unwrap();
        }
        let output = helper.await.unwrap().unwrap();
        assert_eq!(
            output["output"]["data"]["decision"],
            if approve { "accepted" } else { "rejected" }
        );
        assert_eq!(output["output"]["data"]["feedback"], "Keep it scoped");
        assert_eq!(
            runtime
                .get_session(&session.owner, session.id)
                .await
                .unwrap()
                .permission_mode,
            PermissionMode::Allow
        );
        if !approve {
            assert!(output["output"]["content"]
                .as_str()
                .unwrap()
                .contains("Do not execute"));
        }
    }
}

#[tokio::test]
async fn managed_decisions_concurrent_clicks_preserve_the_first_actor_and_settled_response() {
    let (_dir, runtime, session, incarnation, _) = fixture(true, PermissionMode::Allow).await;
    let (_, identity) = active_turn(&runtime, &session, incarnation).await;
    let executor = SandboxToolExecutor::new(Arc::downgrade(&runtime));
    let mut request = question_request("human-1");
    request.turn = Some(identity);
    executor
        .enqueue(&session.owner, session.id, incarnation, &request)
        .await
        .unwrap();
    let approval = only_approval(&runtime, &session).await;
    let first = TurnActor {
        display: Some("Alex".into()),
        ..Default::default()
    };
    let second = TurnActor {
        display: Some("Blair".into()),
        ..Default::default()
    };
    let (a, b) = tokio::join!(
        runtime.decide_approval(
            &session.owner,
            approval.id,
            answer(&["test"], None),
            Some(first)
        ),
        runtime.decide_approval(
            &session.owner,
            approval.id,
            answer(&["live"], None),
            Some(second)
        ),
    );
    assert_ne!(a.is_ok(), b.is_ok());
    let (winner, loser) = match (a, b) {
        (Ok(winner), Err(loser)) | (Err(loser), Ok(winner)) => (winner, loser),
        other => panic!("one winner: {other:?}"),
    };
    assert_eq!(loser.kind(), "already_settled");
    assert_eq!(only_approval(&runtime, &session).await.actor, winner.actor);
    let events = list_events(&runtime.db, &session.owner, session.id, 0, 50)
        .await
        .unwrap();
    assert_eq!(
        events
            .events
            .iter()
            .filter(|event| matches!(event.event, Event::ApprovalResolved { .. }))
            .count(),
        1
    );
    assert_eq!(
        executor
            .service(&session.owner, session.id, incarnation)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn managed_child_binding_advertises_human_tools_with_direct_schemas() {
    let (_dir, runtime, session, incarnation, _) = fixture(true, PermissionMode::Allow).await;
    let binding = list_bindings_for_session(&runtime.db, &session.owner, session.id)
        .await
        .unwrap()
        .remove(0);
    bind_external_session(
        &runtime.db,
        &session.owner,
        binding.grant_id,
        "slack",
        "child/parent/request-key",
        session.id,
    )
    .await
    .unwrap();
    set_session_context(
        &runtime.db,
        &session.owner,
        session.id,
        Some("C-TEST"),
        None,
        Some("request-key"),
    )
    .await
    .unwrap();
    let (_, identity) = active_turn(&runtime, &session, incarnation).await;
    let executor = SandboxToolExecutor::new(Arc::downgrade(&runtime));
    let context = executor
        .bootstrap_context(&session.owner, session.id)
        .await
        .unwrap();
    assert!(context.contains("tb-human MCP"));
    assert!(context.contains("request_plan_approval"));
    assert!(context.contains("single_select"));
    let mut request = question_request("human-1");
    request.turn = Some(identity);
    executor
        .enqueue(&session.owner, session.id, incarnation, &request)
        .await
        .unwrap();
    assert_eq!(
        only_approval(&runtime, &session).await.state,
        ApprovalState::Pending
    );
}

#[tokio::test]
async fn managed_cancellation_tombstones_replays_and_preserves_a_settled_actor() {
    let (_dir, runtime, session, incarnation, _) = fixture(true, PermissionMode::Allow).await;
    let (_, identity) = active_turn(&runtime, &session, incarnation).await;
    let executor = SandboxToolExecutor::new(Arc::downgrade(&runtime));
    let mut request = question_request("cancelled-before-admission");
    request.turn = Some(identity);
    request.cancelled = true;
    executor
        .enqueue(&session.owner, session.id, incarnation, &request)
        .await
        .unwrap();
    executor
        .enqueue(&session.owner, session.id, incarnation, &request)
        .await
        .unwrap();
    assert!(
        list_approvals(&runtime.db, &session.owner, None, Some(session.id))
            .await
            .unwrap()
            .is_empty()
    );
    request.cancelled = false;
    assert!(executor
        .enqueue(&session.owner, session.id, incarnation, &request)
        .await
        .is_err());
    request.cancelled = true;
    request.arguments["questions"][0]["question"] = serde_json::json!("Changed?");
    assert!(executor
        .enqueue(&session.owner, session.id, incarnation, &request)
        .await
        .is_err());
    request.request_id = "cancelled-after-admission".into();
    request.cancelled = false;
    executor
        .enqueue(&session.owner, session.id, incarnation, &request)
        .await
        .unwrap();
    let approval = only_approval(&runtime, &session).await;
    request.cancelled = true;
    executor
        .enqueue(&session.owner, session.id, incarnation, &request)
        .await
        .unwrap();
    assert_eq!(
        only_approval(&runtime, &session).await.state,
        ApprovalState::Abandoned
    );
    assert!(runtime
        .decide_approval(&session.owner, approval.id, answer(&["test"], None), None)
        .await
        .is_err());
    request.request_id = "cancelled-after-answer".into();
    request.cancelled = false;
    executor
        .enqueue(&session.owner, session.id, incarnation, &request)
        .await
        .unwrap();
    let pending = list_approvals(
        &runtime.db,
        &session.owner,
        Some(ApprovalState::Pending),
        Some(session.id),
    )
    .await
    .unwrap()
    .remove(0);
    let actor = TurnActor {
        display: Some("Alex".into()),
        ..Default::default()
    };
    runtime
        .decide_approval(
            &session.owner,
            pending.id,
            answer(&["test"], None),
            Some(actor.clone()),
        )
        .await
        .unwrap();
    request.cancelled = true;
    executor
        .enqueue(&session.owner, session.id, incarnation, &request)
        .await
        .unwrap();
    assert!(executor
        .service(&session.owner, session.id, incarnation)
        .await
        .unwrap()
        .is_empty());
    let settled = runtime
        .get_approval(&session.owner, pending.id)
        .await
        .unwrap();
    assert_eq!(settled.state, ApprovalState::Approved);
    assert_eq!(settled.actor, Some(actor));
    assert_eq!(
        runtime
            .get_session(&session.owner, session.id)
            .await
            .unwrap()
            .lifecycle,
        tidebreak_core::SessionLifecycle::Running
    );
}
