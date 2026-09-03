use super::*;

#[tokio::test]
async fn client_wait_parks_resolves_and_recovers_exactly() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    let _accepted = match store
        .accept_turn(turn_id, chat.id, "gpt-5", "connect a folder")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    let claimed_at = Utc::now();
    let turn_lease = uuid::Uuid::new_v4();
    store
        .claim_turn(
            turn_lease,
            claimed_at,
            claimed_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap()
        .turn
        .unwrap();
    let request = crate::model::ClientToolCallRequest {
        id: CallId::new(),
        chat_id: chat.id,
        turn_id,
        provider_id: "native".into(),
        name: "connect_folder".into(),
        arguments: serde_json::json!({"suggested_name": "Documents"}),
    };
    let progress = crate::model::TurnCheckpointProgress {
        model_steps: 3,
        usage: crate::provider::Usage {
            input_tokens: 11,
            output_tokens: 7,
            cache_read_input_tokens: 5,
            cache_creation_input_tokens: 2,
        },
    };
    assert!(store
        .park_turn_for_client_tool_call(
            turn_id,
            turn_lease,
            0,
            crate::model::TurnCheckpointProgress {
                model_steps: 0,
                usage: crate::provider::Usage::default(),
            },
            Utc::now(),
            &request,
        )
        .await
        .is_err());
    let stale_steer_id = TurnSteerId::new();
    assert!(matches!(
        store
            .accept_turn_steer(
                stale_steer_id,
                turn_id,
                chat.id,
                "apply before native checkpoint",
                false,
            )
            .await
            .unwrap(),
        AcceptTurnSteerOutcome::Accepted(_)
    ));
    let first_steer_at = Utc::now();
    assert!(matches!(
        store
            .apply_turn_steer(
                turn_id,
                turn_lease,
                stale_steer_id,
                1,
                None,
                &[],
                first_steer_at,
            )
            .await
            .unwrap()
            .unwrap()
            .outcome,
        ApplyTurnSteerOutcome::Applied(_)
    ));
    let checkpoint_at = Utc::now();
    assert!(matches!(
        store
            .park_turn_for_client_tool_call(
                turn_id,
                turn_lease,
                0,
                progress,
                checkpoint_at,
                &request,
            )
            .await
            .unwrap()
            .unwrap(),
        ParkTurnForClientCallOutcome::OutputSuperseded(turn)
            if turn.steer_revision == 1
    ));

    let pending_steer_id = TurnSteerId::new();
    assert!(matches!(
        store
            .accept_turn_steer(
                pending_steer_id,
                turn_id,
                chat.id,
                "pending before native checkpoint",
                false,
            )
            .await
            .unwrap(),
        AcceptTurnSteerOutcome::Accepted(_)
    ));
    let pending_checkpoint_at = Utc::now();
    assert!(matches!(
        store
            .park_turn_for_client_tool_call(
                turn_id,
                turn_lease,
                1,
                progress,
                pending_checkpoint_at,
                &request,
            )
            .await
            .unwrap()
            .unwrap(),
        ParkTurnForClientCallOutcome::SteerPending(turn)
            if turn.steer_revision == 1
    ));
    let second_steer_at = Utc::now();
    assert!(matches!(
        store
            .apply_turn_steer(
                turn_id,
                turn_lease,
                pending_steer_id,
                2,
                None,
                &[],
                second_steer_at,
            )
            .await
            .unwrap()
            .unwrap()
            .outcome,
        ApplyTurnSteerOutcome::Applied(_)
    ));

    let parked_at = Utc::now();
    assert_eq!(
        store
            .park_turn_for_client_tool_call(
                turn_id,
                uuid::Uuid::new_v4(),
                2,
                progress,
                parked_at,
                &request,
            )
            .await
            .unwrap(),
        None
    );
    assert!(store.list_tool_calls(chat.id).await.unwrap().is_empty());
    let (parked_turn, parked_call, parked_wait) = match store
        .park_turn_for_client_tool_call(turn_id, turn_lease, 2, progress, parked_at, &request)
        .await
        .unwrap()
        .unwrap()
    {
        ParkTurnForClientCallOutcome::Parked {
            turn, call, wait, ..
        } => (turn, call, wait),
        outcome => panic!("unexpected park outcome: {outcome:?}"),
    };
    assert_eq!(parked_turn.status, TurnRunStatus::WaitingForClient);
    assert_eq!(parked_turn.lease_token, None);
    assert_eq!(parked_turn.model_steps, progress.model_steps);
    assert_eq!(parked_turn.usage, progress.usage);
    assert_eq!(parked_call.status, ToolCallStatus::Pending);
    assert_eq!(
        parked_wait.status,
        crate::model::TurnClientWaitStatus::Waiting
    );
    assert_eq!((parked_wait.attempt_count, parked_wait.claim_count), (1, 1));
    assert_eq!(parked_wait.progress, progress);
    let conflicting_progress = crate::model::TurnCheckpointProgress {
        model_steps: progress.model_steps + 1,
        ..progress
    };
    assert!(matches!(
        store
            .park_turn_for_client_tool_call(
                turn_id,
                turn_lease,
                2,
                conflicting_progress,
                parked_at,
                &request,
            )
            .await
            .unwrap()
            .unwrap(),
        ParkTurnForClientCallOutcome::IdentityConflict
    ));
    assert!(matches!(
        store
            .accept_turn(TurnId::new(), chat.id, "gpt-5", "must stay occupied")
            .await
            .unwrap(),
        AcceptTurnOutcome::ChatBusy(turn) if turn.id == turn_id
    ));

    let executor_id = uuid::Uuid::new_v4();
    let client_lease = uuid::Uuid::new_v4();
    let client_claimed_at = parked_at + chrono::Duration::seconds(1);
    assert!(matches!(
        store
            .claim_client_tool_call(
                request.id,
                chat.id,
                executor_id,
                client_lease,
                client_claimed_at,
                client_claimed_at + chrono::Duration::minutes(1),
            )
            .await
            .unwrap(),
        ClaimClientToolCallOutcome::Claimed(_)
    ));
    let resolved_at = client_claimed_at + chrono::Duration::seconds(1);
    let resolution = ToolCallResolution::Completed {
        result: "root-1".into(),
    };
    let journaled = store
        .resolve_client_tool_call_and_append_event(
            request.id,
            chat.id,
            client_lease,
            resolved_at,
            &resolution,
            resolved_at,
        )
        .await
        .unwrap();
    assert_eq!(journaled.outcome, ResolveToolCallOutcome::Resolved);
    assert_eq!(journaled.terminal_event, None);
    assert_eq!(
        journaled.turn.as_ref().map(|turn| turn.status),
        Some(TurnRunStatus::Resuming)
    );
    // A client call is executed and resolved outside the agent loop, and the
    // resumed loop reads its result straight into the model transcript without
    // ever revisiting the call. Nothing else announces that it finished, so the
    // renderer showed the row running from `ToolCallStarted` until the chat was
    // reopened. It announces itself here instead.
    let completions = |events: Vec<crate::SequencedEvent>| {
        events
            .into_iter()
            .filter(|event| matches!(event.event, AgentEvent::ToolCallCompleted { .. }))
            .collect::<Vec<_>>()
    };
    let announced = completions(store.list_events(chat.id, 0).await.unwrap());
    assert_eq!(announced.len(), 1);
    let AgentEvent::ToolCallCompleted {
        call_id,
        ref output,
        ref action,
        ..
    } = announced[0].event
    else {
        unreachable!("filtered to completions")
    };
    assert_eq!(call_id, request.id);
    assert!(!output.is_error);
    // Projected from the call's own stored arguments, so a client card names
    // its action identically live and after a reload.
    assert_eq!(
        action.as_ref(),
        crate::ToolActionPreview::build(&request.name, &request.arguments).as_ref()
    );

    // An exact retry recovers the same outcome without announcing it twice.
    assert_eq!(
        store
            .resolve_client_tool_call_and_append_event(
                request.id,
                chat.id,
                client_lease,
                resolved_at,
                &resolution,
                resolved_at,
            )
            .await
            .unwrap()
            .outcome,
        ResolveToolCallOutcome::Existing
    );
    assert_eq!(
        completions(store.list_events(chat.id, 0).await.unwrap()).len(),
        1
    );
    let resumable = store.get_turn(turn_id).await.unwrap().unwrap();
    assert_eq!(resumable.status, TurnRunStatus::Resuming);
    assert_eq!((resumable.attempt_count, resumable.claim_count), (1, 1));
    assert_eq!(resumable.model_steps, progress.model_steps);
    assert_eq!(resumable.usage, progress.usage);
    let wait = entities::turn_client_wait::Entity::find_by_id(request.id.0)
        .one(&store.conn)
        .await
        .unwrap()
        .unwrap();
    assert!(entities::turn_client_wait::Entity::update_many()
        .col_expr(
            entities::turn_client_wait::Column::ModelSteps,
            sea_orm::sea_query::Expr::value(0),
        )
        .filter(entities::turn_client_wait::Column::CallId.eq(request.id.0))
        .exec(&store.conn)
        .await
        .is_err());
    assert!(entities::turn_client_wait::Entity::update_many()
        .col_expr(
            entities::turn_client_wait::Column::InputTokens,
            sea_orm::sea_query::Expr::value(i64::from(u32::MAX) + 1),
        )
        .filter(entities::turn_client_wait::Column::CallId.eq(request.id.0))
        .exec(&store.conn)
        .await
        .is_err());
    assert_eq!(
        wait.status,
        crate::model::TurnClientWaitStatus::Resumed.as_str()
    );
    assert_eq!(wait.closed_at, Some(resumable.updated_at));

    assert!(matches!(
        store
            .park_turn_for_client_tool_call(
                turn_id,
                turn_lease,
                2,
                progress,
                Utc::now(),
                &request,
            )
            .await
            .unwrap()
            .unwrap(),
        ParkTurnForClientCallOutcome::Existing { turn, wait, .. }
            if turn.status == TurnRunStatus::Resuming
                && wait.status == crate::model::TurnClientWaitStatus::Resumed
    ));
    let resumed_lease = uuid::Uuid::new_v4();
    let resumed = store
        .claim_turn(
            resumed_lease,
            resolved_at,
            resolved_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap()
        .turn
        .unwrap();
    assert_eq!((resumed.attempt_count, resumed.claim_count), (1, 2));
    assert_eq!(resumed.model_steps, progress.model_steps);
    assert_eq!(resumed.usage, progress.usage);
    let regressing_output = Message {
        id: MessageId::new(),
        chat_id: chat.id,
        turn_id,
        role: Role::Assistant,
        reasoning: Default::default(),
        content: "must not commit".into(),
        llm_content: None,
        created_at: resolved_at + chrono::Duration::microseconds(1),
    };
    assert_eq!(
        store
            .complete_turn_and_append_event(
                turn_id,
                turn_lease,
                2,
                regressing_output.created_at,
                &regressing_output,
                0,
                crate::provider::Usage::default(),
                crate::provider::StopReason::EndTurn,
            )
            .await
            .unwrap(),
        None,
        "a stale claim remains lease-lost even when its proposed usage is lower"
    );
    assert!(store
        .complete_turn_and_append_event(
            turn_id,
            resumed_lease,
            2,
            regressing_output.created_at,
            &regressing_output,
            0,
            crate::provider::Usage::default(),
            crate::provider::StopReason::EndTurn,
        )
        .await
        .is_err());
    assert!(store
        .list_messages(chat.id)
        .await
        .unwrap()
        .iter()
        .all(|message| message.id != regressing_output.id));

    let second_progress = crate::model::TurnCheckpointProgress {
        model_steps: 2,
        usage: crate::provider::Usage {
            input_tokens: 17,
            output_tokens: 9,
            cache_read_input_tokens: 4,
            cache_creation_input_tokens: 1,
        },
    };
    let expected_total_usage = crate::provider::Usage {
        input_tokens: progress.usage.input_tokens + second_progress.usage.input_tokens,
        output_tokens: progress.usage.output_tokens + second_progress.usage.output_tokens,
        cache_read_input_tokens: progress.usage.cache_read_input_tokens
            + second_progress.usage.cache_read_input_tokens,
        cache_creation_input_tokens: progress.usage.cache_creation_input_tokens
            + second_progress.usage.cache_creation_input_tokens,
    };
    let second_request = crate::model::ClientToolCallRequest {
        id: CallId::new(),
        provider_id: "native-second".into(),
        name: "open_file".into(),
        arguments: serde_json::json!({"root_id": "root-1"}),
        ..request.clone()
    };
    let second_parked_at = resolved_at + chrono::Duration::seconds(1);
    let second_parked = store
        .park_turn_for_client_tool_call(
            turn_id,
            resumed_lease,
            2,
            second_progress,
            second_parked_at,
            &second_request,
        )
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        second_parked,
        ParkTurnForClientCallOutcome::Parked { ref turn, ref wait, .. }
            if turn.model_steps == progress.model_steps + second_progress.model_steps
                && turn.usage == expected_total_usage
                && wait.progress == second_progress
    ));
    assert!(matches!(
        store
            .park_turn_for_client_tool_call(
                turn_id,
                resumed_lease,
                2,
                second_progress,
                Utc::now(),
                &second_request,
            )
            .await
            .unwrap()
            .unwrap(),
        ParkTurnForClientCallOutcome::Existing { turn, wait, .. }
            if turn.model_steps == progress.model_steps + second_progress.model_steps
                && turn.usage == expected_total_usage
                && wait.progress == second_progress
    ));
    let twice_parked = store.get_turn(turn_id).await.unwrap().unwrap();
    assert_eq!(
        twice_parked.model_steps,
        progress.model_steps + second_progress.model_steps
    );
    assert_eq!(twice_parked.usage, expected_total_usage);
}

#[tokio::test]
async fn client_wait_accounting_overflow_rolls_back_the_checkpoint() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    let accepted = match store
        .accept_turn(turn_id, chat.id, "gpt-5", "overflow")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    let lease_token = uuid::Uuid::new_v4();
    let claimed_at = accepted.available_at + chrono::Duration::seconds(1);
    store
        .claim_turn(
            lease_token,
            claimed_at,
            claimed_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap();
    entities::code_turn::Entity::update_many()
        .col_expr(
            entities::code_turn::Column::InputTokens,
            sea_orm::sea_query::Expr::value(i64::from(u32::MAX)),
        )
        .filter(entities::code_turn::Column::Id.eq(turn_id.0))
        .exec(&store.conn)
        .await
        .unwrap();
    let request = crate::model::ClientToolCallRequest {
        id: CallId::new(),
        chat_id: chat.id,
        turn_id,
        provider_id: "native".into(),
        name: "connect_folder".into(),
        arguments: serde_json::json!({}),
    };
    assert!(store
        .park_turn_for_client_tool_call(
            turn_id,
            lease_token,
            0,
            crate::model::TurnCheckpointProgress {
                model_steps: 1,
                usage: crate::provider::Usage {
                    input_tokens: 1,
                    ..crate::provider::Usage::default()
                },
            },
            claimed_at + chrono::Duration::seconds(1),
            &request,
        )
        .await
        .is_err());
    assert!(store.list_tool_calls(chat.id).await.unwrap().is_empty());
    assert!(entities::turn_client_wait::Entity::find_by_id(request.id.0)
        .one(&store.conn)
        .await
        .unwrap()
        .is_none());
    let still_running = store.get_turn(turn_id).await.unwrap().unwrap();
    assert_eq!(still_running.status, TurnRunStatus::Running);
    assert_eq!(still_running.lease_token, Some(lease_token));
    assert_eq!(still_running.usage.input_tokens, u32::MAX);
}

async fn park_test_client_wait(
    store: &DbStore,
    chat_id: ChatId,
) -> (TurnId, crate::model::ClientToolCallRequest, DateTime<Utc>) {
    let turn_id = TurnId::new();
    let accepted = match store
        .accept_turn(turn_id, chat_id, "gpt-5", "native action")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    let claimed_at = accepted.available_at + chrono::Duration::seconds(1);
    let turn_lease = uuid::Uuid::new_v4();
    store
        .claim_turn(
            turn_lease,
            claimed_at,
            claimed_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap()
        .turn
        .unwrap();
    let request = crate::model::ClientToolCallRequest {
        id: CallId::new(),
        chat_id,
        turn_id,
        provider_id: "native".into(),
        name: "connect_folder".into(),
        arguments: serde_json::json!({}),
    };
    let parked_at = claimed_at + chrono::Duration::seconds(1);
    assert!(matches!(
        store
            .park_turn_for_client_tool_call(
                turn_id,
                turn_lease,
                0,
                test_checkpoint_progress(),
                parked_at,
                &request,
            )
            .await
            .unwrap()
            .unwrap(),
        ParkTurnForClientCallOutcome::Parked { .. }
    ));
    (turn_id, request, parked_at)
}

async fn park_test_user_questions(
    store: &DbStore,
    chat_id: ChatId,
) -> (TurnId, crate::model::ClientToolCallRequest, DateTime<Utc>) {
    let turn_id = TurnId::new();
    let accepted = match store
        .accept_turn(turn_id, chat_id, "gpt-5", "ask me")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected question turn acceptance: {outcome:?}"),
    };
    let claimed_at = accepted.available_at + chrono::Duration::seconds(1);
    let lease = uuid::Uuid::new_v4();
    store
        .claim_turn(lease, claimed_at, claimed_at + chrono::Duration::minutes(1))
        .await
        .unwrap()
        .turn
        .unwrap();
    let request = crate::model::ClientToolCallRequest {
        id: CallId::new(),
        chat_id,
        turn_id,
        provider_id: "provider-question".into(),
        name: crate::ASK_USER_QUESTIONS_TOOL.into(),
        arguments: serde_json::json!({
            "questions": [
                {
                    "id": "target",
                    "header": "Target",
                    "question": "Where should I deploy?",
                    "options": [
                        {"id": "staging", "label": "Staging", "description": "Deploy for internal verification."},
                        {"id": "production", "label": "Production", "description": "Deploy to customers."}
                    ],
                    "question_type": "multi_select",
                    "allow_free_form": true
                },
                {
                    "id": "note",
                    "header": "Note",
                    "question": "Anything else I should know?",
                    "question_type": "single_select",
                    "allow_free_form": true
                }
            ]
        }),
    };
    let parked_at = claimed_at + chrono::Duration::seconds(1);
    let parked = store
        .park_turn_for_client_tool_call(
            turn_id,
            lease,
            0,
            test_checkpoint_progress(),
            parked_at,
            &request,
        )
        .await
        .unwrap()
        .unwrap();
    match parked {
        ParkTurnForClientCallOutcome::Parked {
            renderer_event:
                Some(SequencedEvent {
                    event:
                        AgentEvent::UserQuestionsAsked {
                            call_id,
                            turn_id: event_turn_id,
                        },
                    ..
                }),
            ..
        } => {
            assert_eq!(call_id, request.id);
            assert_eq!(event_turn_id, turn_id);
        }
        outcome => panic!("unexpected question checkpoint: {outcome:?}"),
    }
    (turn_id, request, parked_at)
}

fn sample_user_answers() -> crate::AnswerUserQuestions {
    crate::AnswerUserQuestions {
        answers: vec![crate::UserQuestionAnswer {
            question_id: "target".into(),
            selected_option_ids: vec!["staging".into(), "production".into()],
            custom_answer: Some("Start with a canary.".into()),
        }],
        additional_user_context: Some("Keep the rollout reversible.".into()),
    }
}

#[tokio::test]
async fn user_questions_survive_reconnect_and_answer_exactly_once() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("questions.db").display()
    );
    let store = DbStore::connect(&url).await.unwrap();
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (turn_id, request, parked_at) = park_test_user_questions(&store, chat.id).await;
    drop(store);

    let restarted = DbStore::connect(&url).await.unwrap();
    let pending = restarted
        .list_pending_user_questions(chat.id)
        .await
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].call_id, request.id);
    assert_eq!(pending[0].turn_id, turn_id);
    assert_eq!(
        pending[0]
            .questions
            .iter()
            .map(|question| question.id.as_str())
            .collect::<Vec<_>>(),
        vec!["target", "note"]
    );
    assert!(matches!(
        restarted
            .claim_client_tool_call(
                request.id,
                chat.id,
                uuid::Uuid::new_v4(),
                uuid::Uuid::new_v4(),
                parked_at + chrono::Duration::seconds(1),
                parked_at + chrono::Duration::minutes(1),
            )
            .await
            .unwrap(),
        ClaimClientToolCallOutcome::Unavailable
    ));

    let answer_request = crate::AnswerUserQuestionsRequest {
        chat_id: chat.id,
        call_id: request.id,
        answers: sample_user_answers(),
    };
    let answered_at = parked_at + chrono::Duration::seconds(1);
    let answered = restarted
        .answer_user_questions(&answer_request, answered_at)
        .await
        .unwrap();
    let crate::AnswerUserQuestionsOutcome::Answered {
        turn,
        completion_event,
        ..
    } = answered
    else {
        panic!("unexpected answer outcome: {answered:?}");
    };
    assert_eq!(turn.id, turn_id);
    assert_eq!(turn.status, TurnRunStatus::Resuming);
    // The answer announces the call's completion itself: the renderer settles
    // the card from this event rather than waiting for the turn to end.
    let crate::AgentEvent::ToolCallCompleted {
        call_id,
        output,
        result: Some(preview),
        ..
    } = &completion_event.event
    else {
        panic!("unexpected completion event: {completion_event:?}");
    };
    assert_eq!(*call_id, request.id);
    assert!(!output.is_error);
    assert_eq!(
        serde_json::from_str::<crate::AnswerUserQuestions>(&output.content).unwrap(),
        sample_user_answers()
    );
    // The recap the transcript shows once the card is gone: option *labels*
    // rather than ids, and a row for the question nobody answered so the card
    // can say it was skipped instead of quietly dropping it.
    let crate::ToolResultPreview::UserQuestions {
        answers,
        additional_context,
    } = preview
    else {
        panic!("unexpected question recap: {preview:?}");
    };
    assert_eq!(
        answers,
        &vec![
            crate::AnsweredUserQuestion {
                question: "Where should I deploy?".into(),
                selected: vec!["Staging".into(), "Production".into()],
                custom_answer: Some("Start with a canary.".into()),
            },
            crate::AnsweredUserQuestion {
                question: "Anything else I should know?".into(),
                selected: Vec::new(),
                custom_answer: None,
            },
        ]
    );
    assert_eq!(
        additional_context.as_deref(),
        Some("Keep the rollout reversible.")
    );
    let answered_call = restarted
        .list_tool_calls(chat.id)
        .await
        .unwrap()
        .into_iter()
        .find(|call| call.id == request.id)
        .unwrap();
    // Rehydration reads the stored column, not the event, so a reload has to
    // find the same recap there.
    assert_eq!(answered_call.result_preview.as_ref(), Some(preview));
    let journaled_completions = restarted
        .list_events(chat.id, 0)
        .await
        .unwrap()
        .into_iter()
        .filter(|event| matches!(event.event, crate::AgentEvent::ToolCallCompleted { .. }))
        .collect::<Vec<_>>();
    assert_eq!(journaled_completions, vec![*completion_event]);
    assert!(restarted
        .list_pending_user_questions(chat.id)
        .await
        .unwrap()
        .is_empty());
    let resumed_lease = uuid::Uuid::new_v4();
    let resumed_at = answered_at + chrono::Duration::seconds(1);
    let resumed = restarted
        .claim_turn(
            resumed_lease,
            resumed_at,
            resumed_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap()
        .turn
        .expect("the exact parked turn must be reclaimable");
    assert_eq!(resumed.id, turn_id);
    assert_eq!((resumed.attempt_count, resumed.claim_count), (1, 2));
    let transcript = restarted
        .get_chat_transcript(chat.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        transcript
            .messages
            .iter()
            .filter(|message| message.role == Role::User)
            .count(),
        1,
        "answering must not create a second user turn"
    );
    assert!(matches!(
        restarted
            .answer_user_questions(&answer_request, answered_at)
            .await
            .unwrap(),
        crate::AnswerUserQuestionsOutcome::Existing(turn)
            if turn.id == turn_id
    ));
    // The exact retry recovered the committed answers without announcing the
    // completion a second time.
    assert_eq!(
        restarted
            .list_events(chat.id, 0)
            .await
            .unwrap()
            .into_iter()
            .filter(|event| matches!(event.event, crate::AgentEvent::ToolCallCompleted { .. }))
            .count(),
        1
    );
    let contradictory = crate::AnswerUserQuestionsRequest {
        answers: crate::AnswerUserQuestions {
            answers: vec![crate::UserQuestionAnswer {
                question_id: "target".into(),
                selected_option_ids: vec!["production".into()],
                custom_answer: None,
            }],
            additional_user_context: Some("Keep the rollout reversible.".into()),
        },
        ..answer_request
    };
    assert_eq!(
        restarted
            .answer_user_questions(&contradictory, answered_at)
            .await
            .unwrap(),
        crate::AnswerUserQuestionsOutcome::AnswerConflict
    );
    let call = restarted
        .list_tool_calls(chat.id)
        .await
        .unwrap()
        .into_iter()
        .find(|call| call.id == request.id)
        .unwrap();
    assert_eq!(call.status, ToolCallStatus::Completed);
    assert_eq!(
        serde_json::from_str::<crate::AnswerUserQuestions>(
            call.result.as_deref().expect("answer result")
        )
        .unwrap(),
        sample_user_answers()
    );
    assert!(matches!(
        restarted
            .request_turn_cancellation(turn_id, resumed_at + chrono::Duration::seconds(1))
            .await
            .unwrap()
            .unwrap(),
        RequestTurnCancellationOutcome::Requested(_)
    ));
    assert!(matches!(
        restarted
            .finish_turn_cancellation(
                turn_id,
                resumed_lease,
                resumed_at + chrono::Duration::seconds(2),
            )
            .await
            .unwrap()
            .unwrap(),
        crate::FinishTurnCancellationOutcome::Cancelled(_)
    ));
    assert!(matches!(
        restarted.delete_chat(chat.id).await.unwrap(),
        crate::DeleteChatOutcome::Deleted { .. }
    ));
}

#[tokio::test]
async fn user_question_answer_validation_and_cancellation_are_closed() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (turn_id, request, parked_at) = park_test_user_questions(&store, chat.id).await;
    let invalid = crate::AnswerUserQuestionsRequest {
        chat_id: chat.id,
        call_id: request.id,
        answers: crate::AnswerUserQuestions {
            answers: vec![crate::UserQuestionAnswer {
                question_id: "target".into(),
                selected_option_ids: vec!["not-an-option".into()],
                custom_answer: None,
            }],
            additional_user_context: None,
        },
    };
    assert_eq!(
        store
            .answer_user_questions(&invalid, parked_at)
            .await
            .unwrap(),
        crate::AnswerUserQuestionsOutcome::InvalidAnswer
    );
    let other_chat = sample_chat();
    store.create_chat(&other_chat).await.unwrap();
    assert_eq!(
        store
            .answer_user_questions(
                &crate::AnswerUserQuestionsRequest {
                    chat_id: other_chat.id,
                    call_id: request.id,
                    answers: sample_user_answers(),
                },
                parked_at,
            )
            .await
            .unwrap(),
        crate::AnswerUserQuestionsOutcome::Unavailable
    );
    assert!(matches!(
        store
            .request_turn_cancellation(turn_id, parked_at + chrono::Duration::seconds(1))
            .await
            .unwrap()
            .unwrap(),
        RequestTurnCancellationOutcome::Cancelled(turn)
            if turn.status == TurnRunStatus::Cancelled
    ));
    assert!(store
        .list_pending_user_questions(chat.id)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        store
            .answer_user_questions(
                &crate::AnswerUserQuestionsRequest {
                    chat_id: chat.id,
                    call_id: request.id,
                    answers: sample_user_answers(),
                },
                parked_at + chrono::Duration::seconds(2),
            )
            .await
            .unwrap(),
        crate::AnswerUserQuestionsOutcome::Unavailable
    );
    assert!(matches!(
        store.delete_chat(chat.id).await.unwrap(),
        crate::DeleteChatOutcome::Deleted { .. }
    ));
}

#[tokio::test]
async fn user_question_answer_and_cancel_race_has_one_serial_outcome() {
    let (_dir, store) = temp_store().await;
    let store = std::sync::Arc::new(store);
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (turn_id, request, parked_at) = park_test_user_questions(&store, chat.id).await;
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    let answer_store = store.clone();
    let answer_barrier = barrier.clone();
    let answer = tokio::spawn(async move {
        answer_barrier.wait().await;
        answer_store
            .answer_user_questions(
                &crate::AnswerUserQuestionsRequest {
                    chat_id: chat.id,
                    call_id: request.id,
                    answers: sample_user_answers(),
                },
                parked_at + chrono::Duration::seconds(1),
            )
            .await
            .unwrap()
    });
    let cancel_store = store.clone();
    let cancel = tokio::spawn(async move {
        barrier.wait().await;
        cancel_store
            .request_turn_cancellation(turn_id, parked_at + chrono::Duration::seconds(1))
            .await
            .unwrap()
            .unwrap()
    });
    let answer = answer.await.unwrap();
    let cancel = cancel.await.unwrap();
    assert!(matches!(
        answer,
        crate::AnswerUserQuestionsOutcome::Answered { .. }
            | crate::AnswerUserQuestionsOutcome::Unavailable
    ));
    assert!(matches!(
        cancel,
        RequestTurnCancellationOutcome::Requested(_)
            | RequestTurnCancellationOutcome::Existing(_)
            | RequestTurnCancellationOutcome::Cancelled(_)
    ));
    assert!(store
        .list_pending_user_questions(chat.id)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        store.get_turn(turn_id).await.unwrap().unwrap().status,
        TurnRunStatus::Cancelled
    );
}

#[tokio::test]
async fn pending_question_projection_serializes_with_answer() {
    let (_dir, store) = temp_store().await;
    let store = std::sync::Arc::new(store);
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (_turn_id, request, parked_at) = park_test_user_questions(&store, chat.id).await;
    let call_id = request.id;
    let request_turn_id = request.turn_id;
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    let list_store = store.clone();
    let list_barrier = barrier.clone();
    let list = tokio::spawn(async move {
        list_barrier.wait().await;
        list_store.list_pending_user_questions(chat.id).await
    });
    let answer_store = store.clone();
    let answer = tokio::spawn(async move {
        barrier.wait().await;
        answer_store
            .answer_user_questions(
                &crate::AnswerUserQuestionsRequest {
                    chat_id: chat.id,
                    call_id,
                    answers: sample_user_answers(),
                },
                parked_at + chrono::Duration::seconds(1),
            )
            .await
    });
    let listed = list
        .await
        .unwrap()
        .expect("projection must not observe drift");
    assert!(
        listed.is_empty()
            || (listed.len() == 1
                && listed[0].call_id == call_id
                && listed[0].turn_id == request_turn_id)
    );
    assert!(matches!(
        answer.await.unwrap().unwrap(),
        crate::AnswerUserQuestionsOutcome::Answered { .. }
    ));
    assert!(store
        .list_pending_user_questions(chat.id)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn user_question_answer_and_worker_claim_race_is_recoverable() {
    let (_dir, store) = temp_store().await;
    let store = std::sync::Arc::new(store);
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (turn_id, request, parked_at) = park_test_user_questions(&store, chat.id).await;
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    let answer_store = store.clone();
    let answer_barrier = barrier.clone();
    let answer = tokio::spawn(async move {
        answer_barrier.wait().await;
        answer_store
            .answer_user_questions(
                &crate::AnswerUserQuestionsRequest {
                    chat_id: chat.id,
                    call_id: request.id,
                    answers: sample_user_answers(),
                },
                parked_at + chrono::Duration::seconds(1),
            )
            .await
            .unwrap()
    });
    let claim_store = store.clone();
    let first_lease = uuid::Uuid::new_v4();
    let claim_at = parked_at + chrono::Duration::seconds(2);
    let claim = tokio::spawn(async move {
        barrier.wait().await;
        claim_store
            .claim_turn(
                first_lease,
                claim_at,
                claim_at + chrono::Duration::minutes(1),
            )
            .await
            .unwrap()
    });
    assert!(matches!(
        answer.await.unwrap(),
        crate::AnswerUserQuestionsOutcome::Answered { turn, .. }
            if turn.id == turn_id && turn.status == TurnRunStatus::Resuming
    ));
    let first_claim = claim.await.unwrap();
    let (claimed, lease) = if let Some(turn) = first_claim.turn {
        (turn, first_lease)
    } else {
        let lease = uuid::Uuid::new_v4();
        let turn = store
            .claim_turn(
                lease,
                claim_at + chrono::Duration::seconds(1),
                claim_at + chrono::Duration::minutes(1),
            )
            .await
            .unwrap()
            .turn
            .expect("answer wake remains durably claimable after an early scan");
        (turn, lease)
    };
    assert_eq!(claimed.id, turn_id);
    assert_eq!((claimed.attempt_count, claimed.claim_count), (1, 2));
    let cancel_at = claim_at + chrono::Duration::seconds(2);
    assert!(matches!(
        store
            .request_turn_cancellation(turn_id, cancel_at)
            .await
            .unwrap()
            .unwrap(),
        RequestTurnCancellationOutcome::Requested(_)
    ));
    assert!(matches!(
        store
            .finish_turn_cancellation(turn_id, lease, cancel_at + chrono::Duration::seconds(1))
            .await
            .unwrap()
            .unwrap(),
        crate::FinishTurnCancellationOutcome::Cancelled(_)
    ));
}

#[tokio::test]
async fn client_wait_schema_rejects_invalid_scope_claim_and_lifecycle() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (turn_id, request, parked_at) = park_test_client_wait(&store, chat.id).await;
    let wait = entities::turn_client_wait::Entity::find_by_id(request.id.0)
        .one(&store.conn)
        .await
        .unwrap()
        .unwrap();

    let second_call = ToolCallRecord {
        id: CallId::new(),
        chat_id: chat.id,
        turn_id,
        provider_id: "native-second".into(),
        name: "connect_folder".into(),
        arguments: serde_json::json!({}),
        raw_arguments: None,
        execution: ToolCallExecution::Client,
        status: ToolCallStatus::Pending,
        result: None,
        result_preview: None,

        provider_replay: None,
        error_code: None,
        error_detail: None,
        client_executor_id: None,
        client_lease_expires_at: None,
        created_at: parked_at + chrono::Duration::microseconds(1),
        resolved_at: None,
    };
    assert!(matches!(
        store.accept_tool_call(&second_call).await.unwrap(),
        AcceptToolCallOutcome::Accepted(_)
    ));

    let valid_second = entities::turn_client_wait::ActiveModel {
        call_id: Set(second_call.id.0),
        turn_id: Set(turn_id.0),
        session_id: Set(chat.id.0),
        park_lease_token: Set(wait.park_lease_token),
        attempt_count: Set(wait.attempt_count),
        claim_count: Set(wait.claim_count),
        model_steps: Set(1),
        input_tokens: Set(0),
        output_tokens: Set(0),
        cache_read_input_tokens: Set(0),
        cache_creation_input_tokens: Set(0),
        status: Set(crate::model::TurnClientWaitStatus::Waiting.as_str().into()),
        parked_at: Set(second_call.created_at),
        closed_at: Set(None),
    };
    assert!(valid_second.clone().insert(&store.conn).await.is_err());

    let mut wrong_claim = valid_second.clone();
    wrong_claim.claim_count = Set(wait.claim_count + 1);
    wrong_claim.status = Set(crate::model::TurnClientWaitStatus::Cancelled
        .as_str()
        .into());
    wrong_claim.closed_at = Set(Some(second_call.created_at));
    assert!(wrong_claim.insert(&store.conn).await.is_err());

    let mut wrong_scope = valid_second.clone();
    wrong_scope.session_id = Set(ChatId::new().0);
    wrong_scope.status = Set(crate::model::TurnClientWaitStatus::Cancelled
        .as_str()
        .into());
    wrong_scope.closed_at = Set(Some(second_call.created_at));
    assert!(wrong_scope.insert(&store.conn).await.is_err());

    let mut missing_close = valid_second.clone();
    missing_close.status = Set(crate::model::TurnClientWaitStatus::Resumed.as_str().into());
    assert!(missing_close.insert(&store.conn).await.is_err());

    let mut close_before_park = valid_second;
    close_before_park.status = Set(crate::model::TurnClientWaitStatus::Cancelled
        .as_str()
        .into());
    close_before_park.closed_at = Set(Some(
        second_call.created_at - chrono::Duration::microseconds(1),
    ));
    assert!(close_before_park.insert(&store.conn).await.is_err());
}

#[tokio::test]
async fn client_wait_cancellation_fences_unclaimed_and_claimed_native_work() {
    let (_dir, store) = temp_store().await;

    let unclaimed_chat = sample_chat();
    store.create_chat(&unclaimed_chat).await.unwrap();
    let (unclaimed_turn, unclaimed_call, unclaimed_parked_at) =
        park_test_client_wait(&store, unclaimed_chat.id).await;
    let cancelled_at = unclaimed_parked_at + chrono::Duration::seconds(1);
    let cancelled = store
        .request_turn_cancellation_and_append_event(unclaimed_turn, cancelled_at)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        cancelled.outcome,
        RequestTurnCancellationOutcome::Cancelled(ref turn)
            if turn.status == TurnRunStatus::Cancelled
                && turn.usage == test_checkpoint_progress().usage
    ));
    assert!(matches!(
        cancelled.terminal_event,
        Some(SequencedEvent {
            event: AgentEvent::TurnCancelled { usage },
            ..
        }) if usage == test_checkpoint_progress().usage
    ));
    assert_eq!(
        store.list_tool_calls(unclaimed_chat.id).await.unwrap()[0].status,
        ToolCallStatus::Cancelled
    );
    let unclaimed_wait = entities::turn_client_wait::Entity::find_by_id(unclaimed_call.id.0)
        .one(&store.conn)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        unclaimed_wait.status,
        crate::model::TurnClientWaitStatus::Cancelled.as_str()
    );

    let claimed_chat = Chat {
        id: ChatId::new(),
        ..sample_chat()
    };
    store.create_chat(&claimed_chat).await.unwrap();
    let (claimed_turn, claimed_call, claimed_parked_at) =
        park_test_client_wait(&store, claimed_chat.id).await;
    let client_claimed_at = claimed_parked_at + chrono::Duration::seconds(1);
    let client_lease = uuid::Uuid::new_v4();
    assert!(matches!(
        store
            .claim_client_tool_call(
                claimed_call.id,
                claimed_chat.id,
                uuid::Uuid::new_v4(),
                client_lease,
                client_claimed_at,
                client_claimed_at + chrono::Duration::minutes(1),
            )
            .await
            .unwrap(),
        ClaimClientToolCallOutcome::Claimed(_)
    ));
    let requested_at = client_claimed_at + chrono::Duration::seconds(1);
    let requested = store
        .request_turn_cancellation_and_append_event(claimed_turn, requested_at)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        requested.outcome,
        RequestTurnCancellationOutcome::Requested(ref turn)
            if turn.status == TurnRunStatus::CancellingClient
    ));
    assert_eq!(requested.terminal_event, None);
    assert!(matches!(
        store
            .accept_turn(
                TurnId::new(),
                claimed_chat.id,
                "gpt-5",
                "must remain occupied",
            )
            .await
            .unwrap(),
        AcceptTurnOutcome::ChatBusy(turn) if turn.id == claimed_turn
    ));

    let resolved_at = client_claimed_at + chrono::Duration::minutes(1);
    let resolution = ToolCallResolution::Cancelled {
        result: "cancelled by user".into(),
    };
    let live = store
        .resolve_client_tool_call_and_append_event(
            claimed_call.id,
            claimed_chat.id,
            client_lease,
            resolved_at,
            &resolution,
            resolved_at,
        )
        .await
        .unwrap();
    assert_eq!(live.outcome, ResolveToolCallOutcome::LeaseLost);
    assert_eq!(live.turn, None);
    assert_eq!(live.terminal_event, None);
    let journaled = store
        .resolve_expired_client_tool_call_and_append_event(
            claimed_call.id,
            claimed_chat.id,
            client_lease,
            resolved_at,
            &resolution,
            resolved_at,
        )
        .await
        .unwrap();
    assert_eq!(journaled.outcome, ResolveToolCallOutcome::Resolved);
    assert_eq!(
        journaled.turn.as_ref().map(|turn| turn.status),
        Some(TurnRunStatus::Cancelled)
    );
    let terminal_event = journaled.terminal_event.clone().unwrap();
    assert!(matches!(
        terminal_event.event,
        AgentEvent::TurnCancelled { usage } if usage == test_checkpoint_progress().usage
    ));
    assert_eq!(
        store.get_turn(claimed_turn).await.unwrap().unwrap().status,
        TurnRunStatus::Cancelled
    );
    let claimed_wait = entities::turn_client_wait::Entity::find_by_id(claimed_call.id.0)
        .one(&store.conn)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        claimed_wait.status,
        crate::model::TurnClientWaitStatus::Cancelled.as_str()
    );
    // The call resolved before the turn was cancelled, and both are announced
    // in that order. Without the completion the renderer would keep showing the
    // native call running underneath a cancelled turn.
    let events = store.list_events(claimed_chat.id, 0).await.unwrap();
    assert_eq!(events.len(), 2);
    assert!(matches!(
        events[0].event,
        AgentEvent::ToolCallCompleted { call_id, .. } if call_id == claimed_call.id
    ));
    assert!(matches!(events[1].event, AgentEvent::TurnCancelled { .. }));
    let recovered = store
        .resolve_expired_client_tool_call_and_append_event(
            claimed_call.id,
            claimed_chat.id,
            client_lease,
            resolved_at,
            &resolution,
            resolved_at,
        )
        .await
        .unwrap();
    assert_eq!(recovered.outcome, ResolveToolCallOutcome::Existing);
    assert_eq!(recovered.terminal_event, Some(terminal_event));
}

#[tokio::test]
async fn concurrent_client_resolution_and_cancellation_do_not_invert_locks() {
    let (_dir, store) = temp_store().await;
    let store = std::sync::Arc::new(store);

    for _ in 0..8 {
        let chat = sample_chat();
        store.create_chat(&chat).await.unwrap();
        let (turn_id, call, parked_at) = park_test_client_wait(&store, chat.id).await;
        let client_token = uuid::Uuid::new_v4();
        let claimed_at = parked_at + chrono::Duration::seconds(1);
        assert!(matches!(
            store
                .claim_client_tool_call(
                    call.id,
                    chat.id,
                    uuid::Uuid::new_v4(),
                    client_token,
                    claimed_at,
                    claimed_at + chrono::Duration::minutes(1),
                )
                .await
                .unwrap(),
            ClaimClientToolCallOutcome::Claimed(_)
        ));

        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let cancel_store = store.clone();
        let cancel_barrier = barrier.clone();
        let cancel_at = claimed_at + chrono::Duration::seconds(1);
        let cancellation = tokio::spawn(async move {
            cancel_barrier.wait().await;
            cancel_store
                .request_turn_cancellation(turn_id, cancel_at)
                .await
        });
        let resolve_store = store.clone();
        let resolution = tokio::spawn(async move {
            barrier.wait().await;
            resolve_store
                .resolve_client_tool_call(
                    call.id,
                    chat.id,
                    client_token,
                    cancel_at,
                    &ToolCallResolution::Completed {
                        result: "connected".into(),
                    },
                    cancel_at,
                )
                .await
        });
        let (cancellation, resolution) =
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                tokio::join!(cancellation, resolution)
            })
            .await
            .expect("client resolution and cancellation must not deadlock");
        assert!(matches!(
            cancellation.unwrap().unwrap().unwrap(),
            RequestTurnCancellationOutcome::Requested(_)
                | RequestTurnCancellationOutcome::Cancelled(_)
        ));
        assert_eq!(
            resolution.unwrap().unwrap(),
            ResolveToolCallOutcome::Resolved
        );
        assert_eq!(
            store.get_turn(turn_id).await.unwrap().unwrap().status,
            TurnRunStatus::Cancelled
        );
    }
}

#[tokio::test]
async fn client_tool_call_is_fenced_by_its_exact_lease() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let created_at = DateTime::<Utc>::from_timestamp(1_700_000_020, 0).unwrap();
    let call = ToolCallRecord {
        id: CallId::new(),
        chat_id: chat.id,
        turn_id: TurnId::new(),
        provider_id: "tu_client".into(),
        name: "select_folder".into(),
        arguments: serde_json::json!({"hint": "Documents"}),
        raw_arguments: None,
        execution: ToolCallExecution::Client,
        status: ToolCallStatus::Pending,
        result: None,
        result_preview: None,

        provider_replay: None,
        error_code: None,
        error_detail: None,
        client_executor_id: None,
        client_lease_expires_at: None,
        created_at,
        resolved_at: None,
    };
    assert!(matches!(
        store.accept_tool_call(&call).await.unwrap(),
        AcceptToolCallOutcome::Accepted(_)
    ));
    assert_eq!(
        store
            .resolve_server_tool_call(
                call.id,
                &ToolCallResolution::Cancelled {
                    result: "not selected".into(),
                },
                created_at + chrono::Duration::seconds(1),
            )
            .await
            .unwrap(),
        ResolveToolCallOutcome::LeaseLost
    );
    assert_eq!(
        store.list_pending_client_tool_calls(chat.id).await.unwrap(),
        vec![call.clone()]
    );

    let executor = uuid::Uuid::new_v4();
    let requested_lease_token = uuid::Uuid::new_v4();
    let claimed_at = created_at + chrono::Duration::seconds(1);
    let first_expiry = claimed_at + chrono::Duration::minutes(1);
    let claimed = match store
        .claim_client_tool_call(
            call.id,
            chat.id,
            executor,
            requested_lease_token,
            claimed_at,
            first_expiry,
        )
        .await
        .unwrap()
    {
        ClaimClientToolCallOutcome::Claimed(claim) => claim,
        outcome => panic!("unexpected claim outcome: {outcome:?}"),
    };
    assert_eq!(claimed.call.client_executor_id, Some(executor));
    let lease_token = claimed.lease_token;
    assert_eq!(lease_token, requested_lease_token);
    assert!(!serde_json::to_string(&claimed.call)
        .unwrap()
        .contains(&lease_token.to_string()));
    assert!(!format!("{claimed:?}").contains(&lease_token.to_string()));
    assert_eq!(claimed.call.client_lease_expires_at, Some(first_expiry));
    assert!(matches!(
        store
            .claim_client_tool_call(
                call.id,
                chat.id,
                executor,
                lease_token,
                claimed_at + chrono::Duration::milliseconds(1),
                first_expiry + chrono::Duration::milliseconds(1),
            )
            .await
            .unwrap(),
        ClaimClientToolCallOutcome::Existing(_)
    ));
    assert_eq!(
        store
            .claim_client_tool_call(
                call.id,
                chat.id,
                executor,
                uuid::Uuid::new_v4(),
                claimed_at,
                first_expiry,
            )
            .await
            .unwrap(),
        ClaimClientToolCallOutcome::Unavailable
    );
    assert_eq!(
        store
            .claim_client_tool_call(
                call.id,
                chat.id,
                uuid::Uuid::new_v4(),
                uuid::Uuid::new_v4(),
                claimed_at,
                first_expiry,
            )
            .await
            .unwrap(),
        ClaimClientToolCallOutcome::Unavailable
    );

    let extended_expiry = first_expiry + chrono::Duration::minutes(1);
    assert_eq!(
        store
            .heartbeat_client_tool_call(
                call.id,
                ChatId::new(),
                lease_token,
                claimed_at + chrono::Duration::seconds(1),
                extended_expiry,
            )
            .await
            .unwrap(),
        HeartbeatClientToolCallOutcome::LeaseLost
    );
    assert_eq!(
        store
            .heartbeat_client_tool_call(
                call.id,
                chat.id,
                lease_token,
                claimed_at + chrono::Duration::seconds(1),
                extended_expiry,
            )
            .await
            .unwrap(),
        HeartbeatClientToolCallOutcome::Extended
    );
    assert_eq!(
        store
            .heartbeat_client_tool_call(
                call.id,
                chat.id,
                lease_token,
                claimed_at + chrono::Duration::seconds(1),
                extended_expiry,
            )
            .await
            .unwrap(),
        HeartbeatClientToolCallOutcome::Existing
    );
    let resolution = ToolCallResolution::Failed {
        result: "folder picker failed".into(),
        error_code: "picker_failed".into(),
        error_detail: Some("native dialog closed unexpectedly".into()),
    };
    let resolved_at = claimed_at + chrono::Duration::seconds(2);
    assert_eq!(
        store
            .resolve_client_tool_call(
                call.id,
                ChatId::new(),
                lease_token,
                resolved_at,
                &resolution,
                resolved_at,
            )
            .await
            .unwrap(),
        ResolveToolCallOutcome::LeaseLost
    );
    assert_eq!(
        store
            .resolve_client_tool_call(
                call.id,
                chat.id,
                uuid::Uuid::new_v4(),
                resolved_at,
                &resolution,
                resolved_at,
            )
            .await
            .unwrap(),
        ResolveToolCallOutcome::LeaseLost
    );
    assert_eq!(
        store
            .resolve_client_tool_call(
                call.id,
                chat.id,
                lease_token,
                resolved_at + chrono::Duration::milliseconds(1),
                &resolution,
                resolved_at + chrono::Duration::milliseconds(1),
            )
            .await
            .unwrap(),
        ResolveToolCallOutcome::Resolved
    );
    assert_eq!(
        store
            .resolve_client_tool_call(
                call.id,
                chat.id,
                lease_token,
                resolved_at,
                &resolution,
                resolved_at,
            )
            .await
            .unwrap(),
        ResolveToolCallOutcome::Existing
    );
    assert_eq!(
        store
            .resolve_client_tool_call(
                call.id,
                ChatId::new(),
                lease_token,
                resolved_at,
                &resolution,
                resolved_at,
            )
            .await
            .unwrap(),
        ResolveToolCallOutcome::LeaseLost
    );
    assert_eq!(
        store
            .resolve_client_tool_call(
                call.id,
                chat.id,
                uuid::Uuid::new_v4(),
                resolved_at,
                &resolution,
                resolved_at,
            )
            .await
            .unwrap(),
        ResolveToolCallOutcome::LeaseLost
    );
    assert_eq!(
        store
            .resolve_server_tool_call(call.id, &resolution, resolved_at)
            .await
            .unwrap(),
        ResolveToolCallOutcome::LeaseLost
    );
    assert!(store
        .list_pending_client_tool_calls(chat.id)
        .await
        .unwrap()
        .is_empty());
    let stored = store.list_tool_calls(chat.id).await.unwrap().pop().unwrap();
    assert_eq!(stored.status, ToolCallStatus::Failed);
    assert_eq!(stored.error_code.as_deref(), Some("picker_failed"));
    assert_eq!(stored.client_executor_id, Some(executor));
    assert_eq!(stored.client_lease_expires_at, None);
}

#[tokio::test]
async fn expired_client_lease_is_not_transferred_implicitly() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let created_at = DateTime::<Utc>::from_timestamp(1_700_000_030, 0).unwrap();
    let call = ToolCallRecord {
        id: CallId::new(),
        chat_id: chat.id,
        turn_id: TurnId::new(),
        provider_id: "tu_picker".into(),
        name: "select_folder".into(),
        arguments: serde_json::json!({}),
        raw_arguments: None,
        execution: ToolCallExecution::Client,
        status: ToolCallStatus::Pending,
        result: None,
        result_preview: None,

        provider_replay: None,
        error_code: None,
        error_detail: None,
        client_executor_id: None,
        client_lease_expires_at: None,
        created_at,
        resolved_at: None,
    };
    store.accept_tool_call(&call).await.unwrap();
    assert!(matches!(
        store.accept_tool_call(&call).await.unwrap(),
        AcceptToolCallOutcome::Existing(_)
    ));
    let first = uuid::Uuid::new_v4();
    let requested_lease_token = uuid::Uuid::new_v4();
    let claimed_at = created_at + chrono::Duration::seconds(1);
    let expiry = claimed_at + chrono::Duration::seconds(5);
    let lease_token = match store
        .claim_client_tool_call(
            call.id,
            chat.id,
            first,
            requested_lease_token,
            claimed_at,
            expiry,
        )
        .await
        .unwrap()
    {
        ClaimClientToolCallOutcome::Claimed(claim) => claim.lease_token,
        outcome => panic!("unexpected claim outcome: {outcome:?}"),
    };
    let after_expiry = expiry + chrono::Duration::seconds(1);
    let recovered_expiry = after_expiry + chrono::Duration::minutes(1);
    assert!(matches!(
        store
            .claim_client_tool_call(
                call.id,
                chat.id,
                first,
                lease_token,
                after_expiry,
                recovered_expiry,
            )
            .await
            .unwrap(),
        ClaimClientToolCallOutcome::Existing(claim)
            if claim.lease_token == lease_token
                && claim.call.client_executor_id == Some(first)
                && claim.call.client_lease_expires_at == Some(recovered_expiry)
    ));
    let after_recovered_expiry = recovered_expiry + chrono::Duration::seconds(1);
    assert_eq!(
        store
            .claim_client_tool_call(
                call.id,
                chat.id,
                uuid::Uuid::new_v4(),
                uuid::Uuid::new_v4(),
                after_recovered_expiry,
                after_recovered_expiry + chrono::Duration::minutes(1),
            )
            .await
            .unwrap(),
        ClaimClientToolCallOutcome::Unavailable
    );
    assert_eq!(
        store
            .resolve_client_tool_call(
                call.id,
                chat.id,
                lease_token,
                after_recovered_expiry,
                &ToolCallResolution::Cancelled {
                    result: "cancelled".into(),
                },
                after_recovered_expiry,
            )
            .await
            .unwrap(),
        ResolveToolCallOutcome::LeaseLost
    );
    let recovered = ToolCallResolution::Cancelled {
        result: "cancelled after native receipt recovery".into(),
    };
    assert_eq!(
        store
            .resolve_expired_client_tool_call(
                call.id,
                chat.id,
                lease_token,
                after_recovered_expiry,
                &recovered,
                after_recovered_expiry,
            )
            .await
            .unwrap(),
        ResolveToolCallOutcome::Resolved
    );
    assert_eq!(
        store
            .resolve_expired_client_tool_call(
                call.id,
                chat.id,
                lease_token,
                after_recovered_expiry,
                &recovered,
                after_expiry,
            )
            .await
            .unwrap(),
        ResolveToolCallOutcome::Existing
    );
}

#[tokio::test]
async fn concurrent_client_claim_has_one_sqlite_winner() {
    let (_dir, store) = temp_store().await;
    let store = std::sync::Arc::new(store);
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let created_at = DateTime::<Utc>::from_timestamp(1_700_000_040, 123_456_789).unwrap();
    let call = ToolCallRecord {
        id: CallId::new(),
        chat_id: chat.id,
        turn_id: TurnId::new(),
        provider_id: "tu_race".into(),
        name: "select_folder".into(),
        arguments: serde_json::json!({}),
        raw_arguments: None,
        execution: ToolCallExecution::Client,
        status: ToolCallStatus::Pending,
        result: None,
        result_preview: None,

        provider_replay: None,
        error_code: None,
        error_detail: None,
        client_executor_id: None,
        client_lease_expires_at: None,
        created_at,
        resolved_at: None,
    };
    store.accept_tool_call(&call).await.unwrap();
    let claim_at = created_at + chrono::Duration::seconds(1);
    let lease_expires_at = claim_at + chrono::Duration::minutes(1);
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(8));
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let store = store.clone();
        let barrier = barrier.clone();
        let executor_id = uuid::Uuid::new_v4();
        let lease_token = uuid::Uuid::new_v4();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            store
                .claim_client_tool_call(
                    call.id,
                    chat.id,
                    executor_id,
                    lease_token,
                    claim_at,
                    lease_expires_at,
                )
                .await
                .unwrap()
        }));
    }
    let mut claimed = 0;
    let mut unavailable = 0;
    for task in tasks {
        match task.await.unwrap() {
            ClaimClientToolCallOutcome::Claimed(_) => claimed += 1,
            ClaimClientToolCallOutcome::Unavailable => unavailable += 1,
            outcome => panic!("unexpected concurrent claim outcome: {outcome:?}"),
        }
    }
    assert_eq!(claimed, 1);
    assert_eq!(unavailable, 7);
}
