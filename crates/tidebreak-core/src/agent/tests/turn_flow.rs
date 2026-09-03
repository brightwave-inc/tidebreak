use super::*;

/// Asks for a tool once, then answers, recording the tool surface each
/// request advertised.
/// The tool surface of one recorded request: the names advertised, and whether
/// the request constrained the model's use of them.
type AdvertisedTools = (Vec<String>, Option<ToolChoice>);

struct ToolSurfaceRecordingProvider {
    calls: AtomicUsize,
    advertised: Arc<Mutex<Vec<AdvertisedTools>>>,
}

#[async_trait]
impl ModelProvider for ToolSurfaceRecordingProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("fake")
    }

    async fn stream(&self, req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        self.advertised.lock().unwrap().push((
            req.tools.iter().map(|tool| tool.name.clone()).collect(),
            req.tool_choice.clone(),
        ));
        let events = if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            vec![
                ProviderEvent::ToolCallStarted {
                    index: 0,
                    id: "call_1".into(),
                    name: "read_file".into(),
                },
                ProviderEvent::ToolCallArgsDelta {
                    index: 0,
                    fragment: r#"{"path":"note.txt"}"#.into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::ToolUse,
                },
            ]
        } else {
            vec![
                ProviderEvent::TextDelta {
                    text: "done".into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ]
        };
        Ok(stream::iter(events).boxed())
    }
}

/// Accepts `tool_choice: none` and calls a tool anyway — the pathological
/// OpenAI-compatible runtime the wrap-up has to survive. Answers with prose
/// only once no tools are advertised at all.
struct ToolChoiceIgnoringProvider {
    calls: AtomicUsize,
    advertised: Arc<Mutex<Vec<AdvertisedTools>>>,
}

#[async_trait]
impl ModelProvider for ToolChoiceIgnoringProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("fake")
    }

    async fn stream(&self, req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        let tools_offered = !req.tools.is_empty();
        self.advertised.lock().unwrap().push((
            req.tools.iter().map(|tool| tool.name.clone()).collect(),
            req.tool_choice.clone(),
        ));
        self.calls.fetch_add(1, Ordering::SeqCst);
        let events = if tools_offered {
            vec![
                ProviderEvent::ToolCallStarted {
                    index: 0,
                    id: format!("call_{}", self.calls.load(Ordering::SeqCst)),
                    name: "read_file".into(),
                },
                ProviderEvent::ToolCallArgsDelta {
                    index: 0,
                    fragment: r#"{"path":"note.txt"}"#.into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::ToolUse,
                },
            ]
        } else {
            vec![
                ProviderEvent::TextDelta {
                    text: "done anyway".into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ]
        };
        Ok(stream::iter(events).boxed())
    }
}

/// Streams a provider replay block beside a tool-only step, then answers,
/// recording the native state each request carried.
struct ReasoningRecordingProvider {
    calls: AtomicUsize,
    seen: Arc<Mutex<Vec<Vec<Value>>>>,
}

#[async_trait]
impl ModelProvider for ReasoningRecordingProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("fake")
    }

    async fn stream(&self, req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        self.seen.lock().unwrap().push(
            req.messages
                .iter()
                .flat_map(|message| message.reasoning.blocks().to_vec())
                .collect(),
        );
        let events = if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            vec![
                ProviderEvent::ReasoningBlock {
                    data: serde_json::json!({
                        "type": "thinking",
                        "thinking": "plan: read the note first",
                        "signature": "sig-1",
                    }),
                },
                ProviderEvent::ToolCallStarted {
                    index: 0,
                    id: "call_1".into(),
                    name: "read_file".into(),
                },
                ProviderEvent::ToolCallArgsDelta {
                    index: 0,
                    fragment: r#"{"path":"note.txt"}"#.into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::ToolUse,
                },
            ]
        } else {
            vec![
                ProviderEvent::TextDelta {
                    text: "done".into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ]
        };
        Ok(stream::iter(events).boxed())
    }
}

struct MixedQuestionWithReasoningProvider {
    calls: AtomicUsize,
    seen: Arc<Mutex<Vec<Vec<Value>>>>,
}

#[async_trait]
impl ModelProvider for MixedQuestionWithReasoningProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("mixed-question-with-reasoning")
    }

    async fn stream(&self, req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        self.seen.lock().unwrap().push(
            req.messages
                .iter()
                .flat_map(|message| message.reasoning.blocks().to_vec())
                .collect(),
        );
        let events = if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            vec![
                ProviderEvent::ReasoningBlock {
                    data: serde_json::json!({
                        "type": "thinking",
                        "thinking": "I should ask which Aurora they mean.",
                        "signature": "sig-mixed-1",
                    }),
                },
                ProviderEvent::TextDelta {
                    text: "I searched but could not find that issue.".into(),
                },
                ProviderEvent::ToolCallStarted {
                    index: 0,
                    id: "ask_1".into(),
                    name: crate::ASK_USER_QUESTIONS_TOOL.into(),
                },
                ProviderEvent::ToolCallArgsDelta {
                    index: 0,
                    fragment: r#"{"questions":[{"id":"target","header":"Target","question":"Which Aurora?","options":[{"id":"aws","label":"AWS Aurora","description":"The database."}]}]}"#.into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::ToolUse,
                },
            ]
        } else {
            vec![
                ProviderEvent::TextDelta {
                    text: "Which Aurora do you mean?".into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ]
        };
        Ok(stream::iter(events).boxed())
    }
}

#[tokio::test]
async fn a_rejected_mixed_control_step_does_not_replay_its_thinking_on_the_retry() {
    let db = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect_test_sqlite_fixture(&format!(
            "sqlite://{}?mode=rwc",
            db.path().join("mixed-question-thinking.db").display()
        ))
        .await
        .unwrap(),
    );
    let chat = Chat {
        id: SessionId::new(),
        project_id: None,
        title: None,
        model: None,
        reasoning_effort: None,
        permission_mode: Some(PermissionMode::Allow),
        network_policy: Default::default(),
        attachment_revision: 0,
        root_attachments: Vec::new(),
        memory_incognito: false,
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    store
        .accept_turn(turn_id, chat.id, "fake", "explain the aurora ttl")
        .await
        .unwrap();
    let lease_token = uuid::Uuid::new_v4();
    let now = Utc::now();
    store
        .claim_turn(lease_token, now, now + chrono::Duration::minutes(1))
        .await
        .unwrap();
    let mut registry = ToolRegistry::new();
    registry.register_validated_foreground_client(
        crate::ask_user_questions_tool_spec(),
        ApprovalClass::ReadOnly,
        crate::validate_ask_user_questions_arguments,
    );
    let seen = Arc::new(Mutex::new(Vec::new()));
    let provider = Arc::new(MixedQuestionWithReasoningProvider {
        calls: AtomicUsize::new(0),
        seen: seen.clone(),
    });
    let agent = Agent::new(
        provider,
        Arc::new(registry),
        store.clone(),
        AgentConfig {
            model: "fake".into(),
            max_steps: 2,
            ..AgentConfig::default()
        },
    )
    .with_durable_steer(lease_token);
    let (tx, rx) = unbounded();
    let outcome = agent
        .run_claimed_turn(&chat, turn_id, MessageId::new(), 1, &tx)
        .await
        .unwrap();
    drop(tx);
    let events = emitted_events(rx.collect().await);
    assert!(
        matches!(
            &outcome,
            AgentTurnOutcome::Completed {
                output,
                stop_reason: StopReason::EndTurn,
                ..
            } if output.content == "Which Aurora do you mean?"
        ),
        "{outcome:?}"
    );
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ToolCallCompleted { output, .. }
            if output.is_error && output.content.contains("must be called alone")
    )));
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert!(
        seen[1].is_empty(),
        "retry must not carry the rejected step's thinking blocks: {seen:?}"
    );
}

#[tokio::test]
async fn turn_runs_a_tool_call_then_finishes() {
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("note.txt"), "hello from disk").unwrap();

    let db = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect_test_sqlite_fixture(&format!(
            "sqlite://{}?mode=rwc",
            db.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let chat = Chat {
        id: SessionId::new(),
        project_id: None,
        title: None,
        model: None,
        reasoning_effort: None,
        permission_mode: None,
        network_policy: Default::default(),
        attachment_revision: 0,
        root_attachments: Vec::new(),
        memory_incognito: false,
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();

    let tools = Arc::new(ToolRegistry::new().with(Box::new(ReadFile)));
    let agent = Agent::new(
        Arc::new(FakeProvider {
            calls: AtomicUsize::new(0),
        }),
        tools,
        store.clone(),
        AgentConfig {
            model: "fake".into(),
            tool_scratch: Some(tool_scratch(workspace.path())),
            ..Default::default()
        },
    );

    let (tx, rx) = unbounded();
    agent.run_turn(&chat, "read note.txt", &tx).await.unwrap();
    drop(tx);
    let events: Vec<AgentEvent> = rx.collect().await;

    // The tool ran against the real workspace file and the turn completed.
    assert!(matches!(
        events.first(),
        Some(AgentEvent::TurnStarted { .. })
    ));
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::ToolCallStarted { name, .. } if name == "read_file"
    )));
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::ToolCallCompleted { output, .. }
            if output.content == "hello from disk" && !output.is_error
    )));
    assert!(events
        .iter()
        .any(|e| matches!(e, AgentEvent::TextDelta { text } if text == "done")));
    // TurnCompleted usage sums both model calls (5+3 in, 2+4 out).
    let usage = events.iter().find_map(|e| match e {
        AgentEvent::TurnCompleted { usage, .. } => Some(*usage),
        _ => None,
    });
    assert_eq!(
        usage.map(|u| (u.input_tokens, u.output_tokens)),
        Some((8, 6))
    );

    // User input and the final answer are text messages; the tool call is
    // a structured row (not Role::Tool).
    let stored = store.list_messages(chat.id).await.unwrap();
    let roles: Vec<Role> = stored.iter().map(|m| m.role).collect();
    assert_eq!(roles, vec![Role::User, Role::Assistant]);
    let calls = store.list_tool_calls(chat.id).await.unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "read_file");
    assert_eq!(calls[0].result.as_deref(), Some("hello from disk"));
    assert_eq!(calls[0].status, ToolCallStatus::Completed);
    assert!(calls[0].resolved_at.is_some());
}

#[tokio::test]
async fn claimed_turn_defers_terminal_publication_to_durable_worker() {
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("note.txt"), "hello from disk").unwrap();

    let db = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect_test_sqlite_fixture(&format!(
            "sqlite://{}?mode=rwc",
            db.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let chat = Chat {
        id: SessionId::new(),
        project_id: None,
        title: None,
        model: None,
        reasoning_effort: None,
        permission_mode: None,
        network_policy: Default::default(),
        attachment_revision: 0,
        root_attachments: Vec::new(),
        memory_incognito: false,
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    store
        .accept_turn(turn_id, chat.id, "fake", "read note.txt")
        .await
        .unwrap();
    let claimed_at = Utc::now();
    let lease_token = uuid::Uuid::new_v4();
    let claimed = store
        .claim_turn(
            lease_token,
            claimed_at,
            claimed_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap()
        .turn
        .expect("accepted turn is claimable");
    assert_eq!(claimed.id, turn_id);

    let agent = Agent::new(
        Arc::new(FakeProvider {
            calls: AtomicUsize::new(0),
        }),
        Arc::new(ToolRegistry::new().with(Box::new(ReadFile))),
        store.clone(),
        AgentConfig {
            model: "fake".into(),
            tool_scratch: Some(tool_scratch(workspace.path())),
            ..Default::default()
        },
    );
    let output_message_id = MessageId::new();
    let (tx, rx) = unbounded();
    let outcome = agent
        .run_claimed_turn(&chat, turn_id, output_message_id, 1, &tx)
        .await
        .unwrap();
    drop(tx);
    let events = emitted_events(rx.collect().await);

    let AgentTurnOutcome::Completed {
        output,
        usage,
        stop_reason,
        model_steps,
        ..
    } = outcome
    else {
        panic!("claimed turn should complete");
    };
    assert_eq!(output.id, output_message_id);
    assert_eq!(output.chat_id, chat.id);
    assert_eq!(output.turn_id, turn_id);
    assert_eq!(output.role, Role::Assistant);
    assert_eq!(output.content, "done");
    assert_eq!((usage.input_tokens, usage.output_tokens), (8, 6));
    assert_eq!(stop_reason, StopReason::EndTurn);
    assert!(
        events.iter().all(|event| !matches!(
            event,
            AgentEvent::TurnStarted { .. }
                | AgentEvent::TurnCompleted { .. }
                | AgentEvent::TurnCancelled { .. }
        )),
        "the worker owns lifecycle events around the durable execution boundary"
    );

    let stored = store.list_messages(chat.id).await.unwrap();
    assert_eq!(stored.len(), 1, "accepted input must not be duplicated");
    assert_eq!(stored[0].role, Role::User);
    assert_eq!(stored[0].content, "read note.txt");
    assert!(
        stored.iter().all(|message| message.id != output_message_id),
        "final output must remain unpublished until atomic completion"
    );
    let calls = store.list_tool_calls(chat.id).await.unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].turn_id, turn_id);

    for (index, event) in events.iter().enumerate() {
        let ordinal = i32::try_from(index + 1).unwrap();
        assert_eq!(
            store
                .append_turn_event(chat.id, turn_id, lease_token, ordinal, Utc::now(), event,)
                .await
                .unwrap(),
            Some(i64::from(ordinal))
        );
    }

    let completed = store
        .complete_turn_and_append_event(
            turn_id,
            lease_token,
            0,
            Utc::now(),
            &output,
            i32::try_from(model_steps).unwrap(),
            usage,
            stop_reason,
        )
        .await
        .unwrap()
        .expect("the live worker lease can publish its prepared output");
    assert!(matches!(
        completed.outcome,
        crate::CompleteTurnRunOutcome::Completed(_)
    ));
    let terminal = completed
        .terminal_event
        .expect("completion must return its committed terminal event");
    assert_eq!(terminal.seq, i64::try_from(events.len() + 1).unwrap());
    assert_eq!(
        terminal.event,
        AgentEvent::TurnCompleted { usage, stop_reason }
    );
    assert_eq!(
        store.list_events(chat.id, 0).await.unwrap().last(),
        Some(&terminal)
    );
    let recovered = store
        .complete_turn_and_append_event(
            turn_id,
            lease_token,
            0,
            claimed_at + chrono::Duration::hours(1),
            &output,
            i32::try_from(model_steps).unwrap(),
            usage,
            stop_reason,
        )
        .await
        .unwrap()
        .expect("an exact completion retry must remain recoverable");
    assert!(matches!(
        recovered.outcome,
        crate::CompleteTurnRunOutcome::Existing(_)
    ));
    assert_eq!(recovered.terminal_event, Some(terminal));
    let stored = store.list_messages(chat.id).await.unwrap();
    assert_eq!(stored.len(), 2);
    assert_eq!(stored[1].id, output.id);
    assert_eq!(stored[1].chat_id, output.chat_id);
    assert_eq!(stored[1].turn_id, output.turn_id);
    assert_eq!(stored[1].role, output.role);
    assert_eq!(stored[1].content, output.content);
    assert_eq!(
        stored[1].created_at.timestamp_micros(),
        output.created_at.timestamp_micros()
    );

    let failed_turn_id = TurnId::new();
    store
        .accept_turn(
            failed_turn_id,
            chat.id,
            "fake",
            "fail before calling the model",
        )
        .await
        .unwrap();
    let failure_claimed_at = Utc::now();
    let failure_token = uuid::Uuid::new_v4();
    let failed_claim = store
        .claim_turn(
            failure_token,
            failure_claimed_at,
            failure_claimed_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap()
        .turn
        .expect("second accepted turn is claimable");
    assert_eq!(failed_claim.id, failed_turn_id);
    let failing_agent = Agent::new(
        Arc::new(FakeProvider {
            calls: AtomicUsize::new(0),
        }),
        Arc::new(ToolRegistry::new()),
        store.clone(),
        AgentConfig {
            model: "fake".into(),
            ..Default::default()
        },
    );
    let (failure_tx, failure_rx) = unbounded();
    // An invalid first event ordinal fails execution before any event.
    let error = failing_agent
        .run_claimed_turn(&chat, failed_turn_id, MessageId::new(), 0, &failure_tx)
        .await
        .expect_err("the identity guard fails execution");
    drop(failure_tx);
    let failure_events = emitted_events(failure_rx.collect().await);
    assert!(failure_events.iter().all(|event| !matches!(
        event,
        AgentEvent::TurnStarted { .. }
            | AgentEvent::TurnCompleted { .. }
            | AgentEvent::TurnCancelled { .. }
            | AgentEvent::TurnFailed { .. }
    )));
    let error_detail = error.to_string();
    let failure = store
        .record_turn_failure_and_append_event(
            failed_turn_id,
            failure_token,
            Utc::now(),
            crate::TurnFailureRetry::Permanent,
            0,
            Usage::default(),
            "agent_error",
            Some(&error_detail),
        )
        .await
        .unwrap()
        .expect("the worker can record failure before publishing its event");
    assert!(matches!(
        failure.outcome,
        crate::RecordTurnFailureOutcome::Recorded(_)
    ));
    let terminal = failure
        .terminal_event
        .expect("terminal failure must return its committed event");
    assert_eq!(
        terminal.event,
        AgentEvent::TurnFailed {
            error: crate::AgentErrorInfo {
                kind: "agent_error".into(),
                message: error_detail.clone(),
            }
        }
    );
    assert_eq!(
        store.list_events(chat.id, 0).await.unwrap().last(),
        Some(&terminal)
    );
    let recovered = store
        .record_turn_failure_and_append_event(
            failed_turn_id,
            failure_token,
            failure_claimed_at + chrono::Duration::hours(1),
            crate::TurnFailureRetry::Permanent,
            0,
            Usage::default(),
            "agent_error",
            Some(&error_detail),
        )
        .await
        .unwrap()
        .expect("an exact terminal failure retry must remain recoverable");
    assert!(matches!(
        recovered.outcome,
        crate::RecordTurnFailureOutcome::Existing(_)
    ));
    assert_eq!(recovered.terminal_event, Some(terminal));

    let cancelled_turn_id = TurnId::new();
    store
        .accept_turn(
            cancelled_turn_id,
            chat.id,
            "fake",
            "cancel before calling the model",
        )
        .await
        .unwrap();
    let cancellation_claimed_at = Utc::now();
    let cancellation_token = uuid::Uuid::new_v4();
    let cancelled_claim = store
        .claim_turn(
            cancellation_token,
            cancellation_claimed_at,
            cancellation_claimed_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap()
        .turn
        .expect("third accepted turn is claimable");
    assert_eq!(cancelled_claim.id, cancelled_turn_id);
    assert!(matches!(
        store
            .request_turn_cancellation_and_append_event(cancelled_turn_id, Utc::now())
            .await
            .unwrap(),
        Some(crate::JournaledTurnOutcome {
            outcome: crate::RequestTurnCancellationOutcome::Requested(_),
            terminal_event: None,
        })
    ));

    let cancel = CancelToken::new();
    cancel.cancel();
    let cancelled_agent = Agent::new(
        Arc::new(FakeProvider {
            calls: AtomicUsize::new(0),
        }),
        Arc::new(ToolRegistry::new()),
        store.clone(),
        AgentConfig {
            model: "fake".into(),
            ..Default::default()
        },
    )
    .with_cancel(cancel);
    let (cancellation_tx, cancellation_rx) = unbounded();
    let outcome = cancelled_agent
        .run_claimed_turn(
            &chat,
            cancelled_turn_id,
            MessageId::new(),
            1,
            &cancellation_tx,
        )
        .await
        .unwrap();
    drop(cancellation_tx);
    assert_eq!(
        outcome,
        AgentTurnOutcome::Cancelled {
            output: None,
            citations: Vec::new(),
            usage: Usage::default(),
            model_steps: 0,
        }
    );
    let cancellation_events = emitted_events(cancellation_rx.collect().await);
    assert!(cancellation_events.iter().all(|event| !matches!(
        event,
        AgentEvent::TurnStarted { .. }
            | AgentEvent::TurnCompleted { .. }
            | AgentEvent::TurnCancelled { .. }
            | AgentEvent::TurnFailed { .. }
    )));
    let cancellation = store
        .finish_turn_cancellation_and_append_event(
            cancelled_turn_id,
            cancellation_token,
            Utc::now(),
            0,
            Usage::default(),
            None,
            &[],
        )
        .await
        .unwrap()
        .expect("the exact worker acknowledgement must commit");
    assert!(matches!(
        cancellation.outcome,
        crate::FinishTurnCancellationOutcome::Cancelled(_)
    ));
    let terminal = cancellation
        .terminal_event
        .expect("terminal cancellation must return its committed event");
    assert_eq!(
        terminal.event,
        AgentEvent::TurnCancelled {
            usage: Usage::default()
        }
    );
    assert_eq!(
        store.list_events(chat.id, 0).await.unwrap().last(),
        Some(&terminal)
    );
    let recovered = store
        .finish_turn_cancellation_and_append_event(
            cancelled_turn_id,
            cancellation_token,
            cancellation_claimed_at + chrono::Duration::hours(1),
            0,
            Usage::default(),
            None,
            &[],
        )
        .await
        .unwrap()
        .expect("an exact cancellation retry must remain recoverable");
    assert!(matches!(
        recovered.outcome,
        crate::FinishTurnCancellationOutcome::Existing(_)
    ));
    assert_eq!(recovered.terminal_event, Some(terminal));
}

#[tokio::test]
async fn tool_context_inherits_the_chats_project_scope() {
    let db = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect_test_sqlite_fixture(&format!(
            "sqlite://{}?mode=rwc",
            db.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let project = Project {
        id: ProjectId::new(),
        title: None,
        attachment_revision: 0,
        root_attachments: Vec::new(),
        created_at: Utc::now(),
    };
    store.create_project(&project).await.unwrap();
    let chat = Chat {
        id: SessionId::new(),
        project_id: Some(project.id),
        title: None,
        model: None,
        reasoning_effort: None,
        permission_mode: None,
        network_policy: Default::default(),
        attachment_revision: 0,
        root_attachments: Vec::new(),
        memory_incognito: false,
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();
    let observed_project = Arc::new(Mutex::new(None));
    let observed_call = Arc::new(Mutex::new(None));
    let tools = Arc::new(ToolRegistry::new().with(Box::new(ContextRecordingTool {
        observed_project: observed_project.clone(),
        observed_call: observed_call.clone(),
    })));
    let agent = Agent::new(
        Arc::new(FakeProvider {
            calls: AtomicUsize::new(0),
        }),
        tools,
        store,
        AgentConfig {
            model: "fake".into(),
            ..Default::default()
        },
    );

    let (tx, _rx) = unbounded();
    agent.run_turn(&chat, "inspect context", &tx).await.unwrap();
    assert_eq!(*observed_project.lock().unwrap(), Some(Some(project.id)));
    assert!(
        observed_call.lock().unwrap().is_some(),
        "provider adapters need the canonical call id for reconciliation"
    );
}

/// The step budget used to be a cliff: a turn whose last budgeted step
/// asked for a tool failed with `max_steps_exceeded`, throwing away both the
/// tool work and any prose the reader could already see on screen. The
/// budget now bounds tool rounds only — one further model call, made with no
/// tools advertised so it cannot ask for another round, closes the turn with
/// a real answer.
#[tokio::test]
async fn a_turn_at_the_step_ceiling_concludes_with_an_answer() {
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("note.txt"), "secret").unwrap();
    let db = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect_test_sqlite_fixture(&format!(
            "sqlite://{}?mode=rwc",
            db.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let chat = Chat {
        id: SessionId::new(),
        project_id: None,
        title: None,
        model: None,
        reasoning_effort: None,
        permission_mode: None,
        network_policy: Default::default(),
        attachment_revision: 0,
        root_attachments: Vec::new(),
        memory_incognito: false,
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();

    // One step of budget, and step 0 asks for a tool: the turn is at its
    // ceiling the moment that call comes back.
    let advertised = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        Arc::new(ToolSurfaceRecordingProvider {
            calls: AtomicUsize::new(0),
            advertised: advertised.clone(),
        }),
        Arc::new(ToolRegistry::new().with(Box::new(ReadFile))),
        store.clone(),
        AgentConfig {
            model: "fake".into(),
            max_steps: 1,
            ..Default::default()
        },
    );

    let (tx, rx) = unbounded();
    agent.run_turn(&chat, "read note.txt", &tx).await.unwrap();
    drop(tx);
    let events: Vec<AgentEvent> = rx.collect().await;

    assert!(
        matches!(events.last(), Some(AgentEvent::TurnCompleted { .. })),
        "the ceiling must not end the turn as a failure: {events:?}"
    );
    // The last budgeted step's tool still ran, and the closing answer was
    // written with its result in hand.
    assert!(events
        .iter()
        .any(|event| matches!(event, AgentEvent::ToolCallCompleted { .. })));
    let messages = store.list_messages(chat.id).await.unwrap();
    assert_eq!(
        messages
            .last()
            .map(|message| (message.role, message.content.as_str())),
        Some((Role::Assistant, "done")),
        "the reader keeps a real answer: {messages:?}"
    );
    // The wrap-up forbids a tool call rather than withholding the schemas.
    // Tools render at the front of the request, so an empty array would share
    // no cached prefix with any step of the turn — a full-price read of the
    // largest transcript the turn ever sends, to buy a termination guarantee
    // `tool_choice` already gives.
    let advertised = advertised.lock().unwrap().clone();
    assert_eq!(advertised.len(), 2, "one tool step, then the wrap-up");
    assert_eq!(advertised[0].0, advertised[1].0);
    assert!(!advertised[0].0.is_empty());
    assert_eq!(advertised[0].1, None);
    assert_eq!(advertised[1].1, Some(ToolChoice::None));
}

/// `tool_choice` without a tool array is a pairing providers reject, and the
/// wrap-up is the one step that exists to guarantee the turn an answer — so a
/// hard failure there costs the reader everything. A chat-only model advertises
/// no tools, which already makes the step terminal by construction, so the
/// control is withheld rather than sent alone.
#[tokio::test]
async fn a_chat_only_wrap_up_sends_no_tool_choice_and_still_answers() {
    let db = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect_test_sqlite_fixture(&format!(
            "sqlite://{}?mode=rwc",
            db.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let chat = Chat {
        id: SessionId::new(),
        project_id: None,
        title: None,
        model: None,
        reasoning_effort: None,
        permission_mode: None,
        network_policy: Default::default(),
        attachment_revision: 0,
        root_attachments: Vec::new(),
        memory_incognito: false,
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();

    // No budget at all, so the very first call is the wrap-up.
    let advertised = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        Arc::new(ToolSurfaceRecordingProvider {
            // Start on the provider's answer branch: a chat-only model has
            // nothing to call.
            calls: AtomicUsize::new(1),
            advertised: advertised.clone(),
        }),
        Arc::new(ToolRegistry::new().with(Box::new(ReadFile))),
        store.clone(),
        AgentConfig {
            model: "chat-only".into(),
            tools_supported: false,
            max_steps: 0,
            ..Default::default()
        },
    );

    let (tx, rx) = unbounded();
    agent.run_turn(&chat, "just answer", &tx).await.unwrap();
    drop(tx);
    let events: Vec<AgentEvent> = rx.collect().await;

    assert!(
        matches!(events.last(), Some(AgentEvent::TurnCompleted { .. })),
        "the chat-only wrap-up must still produce an answer: {events:?}"
    );
    assert_eq!(
        advertised.lock().unwrap().as_slice(),
        &[(Vec::<String>::new(), None)],
        "no tools and therefore no tool_choice"
    );
    let messages = store.list_messages(chat.id).await.unwrap();
    assert_eq!(
        messages
            .last()
            .map(|message| (message.role, message.content.as_str())),
        Some((Role::Assistant, "done")),
    );
}

/// `tool_choice: none` is a request, not a guarantee: an OpenAI-compatible
/// runtime may accept it and call a tool regardless. That leaves the wrap-up
/// with declined calls and no prose, which is not an answer — the turn would
/// fail as empty and the worker's retry would re-enter the same wrap-up and
/// burn another attempt. One retry with no tools on the request is terminal by
/// construction rather than by the provider's cooperation.
#[tokio::test]
async fn a_wrap_up_a_provider_answers_with_a_tool_call_retries_without_tools() {
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("note.txt"), "secret").unwrap();
    let db = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect_test_sqlite_fixture(&format!(
            "sqlite://{}?mode=rwc",
            db.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let chat = Chat {
        id: SessionId::new(),
        project_id: None,
        title: None,
        model: None,
        reasoning_effort: None,
        permission_mode: None,
        network_policy: Default::default(),
        attachment_revision: 0,
        root_attachments: Vec::new(),
        memory_incognito: false,
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();

    let advertised = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        Arc::new(ToolChoiceIgnoringProvider {
            calls: AtomicUsize::new(0),
            advertised: advertised.clone(),
        }),
        Arc::new(ToolRegistry::new().with(Box::new(ReadFile))),
        store.clone(),
        AgentConfig {
            model: "fake".into(),
            max_steps: 1,
            ..Default::default()
        },
    );

    let (tx, rx) = unbounded();
    agent.run_turn(&chat, "read note.txt", &tx).await.unwrap();
    drop(tx);
    let events: Vec<AgentEvent> = rx.collect().await;

    assert!(
        matches!(events.last(), Some(AgentEvent::TurnCompleted { .. })),
        "the retry closes the turn instead of failing it as empty: {events:?}"
    );
    let messages = store.list_messages(chat.id).await.unwrap();
    assert_eq!(
        messages
            .last()
            .map(|message| (message.role, message.content.as_str())),
        Some((Role::Assistant, "done anyway")),
    );
    let advertised = advertised.lock().unwrap().clone();
    assert_eq!(
        advertised.len(),
        3,
        "one tool step, the ordinary wrap-up, then the retry: {advertised:?}"
    );
    // The ordinary wrap-up is tried first and keeps the cache: same tools, only
    // the choice constrained. Only the retry pays the empty array.
    assert_eq!(advertised[1].0, advertised[0].0);
    assert_eq!(advertised[1].1, Some(ToolChoice::None));
    assert!(
        advertised[2].0.is_empty(),
        "the retry is structurally terminal: {advertised:?}"
    );
    assert_eq!(advertised[2].1, None);
}

#[tokio::test]
async fn a_chat_only_model_never_receives_tool_schemas() {
    let db = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect_test_sqlite_fixture(&format!(
            "sqlite://{}?mode=rwc",
            db.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let chat = Chat {
        id: SessionId::new(),
        project_id: None,
        title: None,
        model: None,
        reasoning_effort: None,
        permission_mode: None,
        network_policy: Default::default(),
        attachment_revision: 0,
        root_attachments: Vec::new(),
        memory_incognito: false,
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();

    let advertised = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        Arc::new(ToolSurfaceRecordingProvider {
            // Start on the provider's answer branch: this test is about the
            // outbound capability surface, not tool execution.
            calls: AtomicUsize::new(1),
            advertised: advertised.clone(),
        }),
        Arc::new(ToolRegistry::new().with(Box::new(ReadFile))),
        store,
        AgentConfig {
            model: "chat-only".into(),
            tools_supported: false,
            ..Default::default()
        },
    );

    let (tx, _rx) = unbounded();
    agent
        .run_turn(&chat, "answer without tools", &tx)
        .await
        .unwrap();
    assert_eq!(
        advertised.lock().unwrap().as_slice(),
        &[(Vec::<String>::new(), None)]
    );
}

/// A provider replay block streamed on a tool-only step must gain a durable
/// empty assistant carrier, ride verbatim into the next request, and survive
/// the turn. Whether it goes on the wire is then the adapter's call — see the
/// router's replay gate.
#[tokio::test]
async fn tool_only_provider_replay_survives_the_turn_that_produced_it() {
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("note.txt"), "secret").unwrap();
    let db = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect_test_sqlite_fixture(&format!(
            "sqlite://{}?mode=rwc",
            db.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let chat = Chat {
        id: SessionId::new(),
        project_id: None,
        title: None,
        model: None,
        reasoning_effort: None,
        permission_mode: None,
        network_policy: Default::default(),
        attachment_revision: 0,
        root_attachments: Vec::new(),
        memory_incognito: false,
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();

    let seen = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        Arc::new(ReasoningRecordingProvider {
            calls: AtomicUsize::new(0),
            seen: seen.clone(),
        }),
        Arc::new(ToolRegistry::new().with(Box::new(ReadFile))),
        store.clone(),
        AgentConfig {
            model: "fake".into(),
            tool_scratch: Some(tool_scratch(workspace.path())),
            ..Default::default()
        },
    );

    let (tx, rx) = unbounded();
    agent.run_turn(&chat, "read note.txt", &tx).await.unwrap();
    drop(tx);
    let events: Vec<AgentEvent> = rx.collect().await;
    assert!(
        matches!(events.last(), Some(AgentEvent::TurnCompleted { .. })),
        "{events:?}"
    );

    let block = serde_json::json!({
        "type": "thinking",
        "thinking": "plan: read the note first",
        "signature": "sig-1",
    });
    {
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2, "one tool step, then the answer step");
        assert!(seen[0].is_empty(), "nothing to replay on the first call");
        assert_eq!(
            seen[1],
            vec![block.clone()],
            "the block reaches the next step exactly as streamed"
        );
    }

    // A second turn on a fresh agent over the same store is the reload:
    // every message comes back off disk.
    let reloaded = Agent::new(
        Arc::new(ReasoningRecordingProvider {
            calls: AtomicUsize::new(1),
            seen: seen.clone(),
        }),
        Arc::new(ToolRegistry::new().with(Box::new(ReadFile))),
        store.clone(),
        AgentConfig {
            model: "fake".into(),
            tool_scratch: Some(tool_scratch(workspace.path())),
            ..Default::default()
        },
    );
    let (tx, rx) = unbounded();
    reloaded.run_turn(&chat, "and again", &tx).await.unwrap();
    drop(tx);
    let _: Vec<AgentEvent> = rx.collect().await;
    assert_eq!(
        seen.lock().unwrap()[2],
        vec![block],
        "the persisted block comes back on the rebuilt transcript"
    );
}

/// Counts every execution so a test can prove a fenced tool never ran.
struct SpyTool {
    ran: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for SpyTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "spy".into(),
            description: "records whether it executed".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }
    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::ReadOnly
    }
    async fn execute(&self, _ctx: &ToolCtx, _args: Value) -> Result<ToolOutput> {
        self.ran.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutput::text("spied"))
    }
}

/// Asks for the `spy` tool once, but first lets the turn's lease be stolen
/// while this provider call is in flight: a fresh claim scan past the lease
/// expiry starts the retry attempt under a new token.
struct LeaseStealingProvider {
    store: Arc<dyn Store>,
    steal_at: DateTime<Utc>,
    stole: AtomicUsize,
}

#[async_trait]
impl ModelProvider for LeaseStealingProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("lease-steal")
    }
    async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        if self.stole.fetch_add(1, Ordering::SeqCst) == 0 {
            let outcome = self
                .store
                .claim_turn(
                    uuid::Uuid::new_v4(),
                    self.steal_at,
                    self.steal_at + chrono::Duration::minutes(1),
                )
                .await?;
            assert!(
                outcome.turn.is_some(),
                "expired turn should be reclaimed for a retry by the steal"
            );
        }
        Ok(stream::iter(vec![
            ProviderEvent::ToolCallStarted {
                index: 0,
                id: "call_spy".into(),
                name: "spy".into(),
            },
            ProviderEvent::ToolCallArgsDelta {
                index: 0,
                fragment: "{}".into(),
            },
            ProviderEvent::Stop {
                reason: StopReason::ToolUse,
            },
        ])
        .boxed())
    }
}

struct AnswerOnlyProvider;

#[async_trait]
impl ModelProvider for AnswerOnlyProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("answer-only")
    }

    async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        Ok(stream::iter(vec![
            ProviderEvent::TextDelta {
                text: "recovered".into(),
            },
            ProviderEvent::Stop {
                reason: StopReason::EndTurn,
            },
        ])
        .boxed())
    }
}

struct RefusalProvider(Vec<ProviderEvent>);

#[async_trait]
impl ModelProvider for RefusalProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("refusal")
    }

    async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        Ok(stream::iter(self.0.clone()).boxed())
    }
}

async fn run_claimed_refusal(events: Vec<ProviderEvent>) -> (AgentTurnOutcome, Vec<AgentEvent>) {
    let db = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect_test_sqlite_fixture(&format!(
            "sqlite://{}?mode=rwc",
            db.path().join("refusal.db").display()
        ))
        .await
        .unwrap(),
    );
    let chat = Chat {
        id: SessionId::new(),
        project_id: None,
        title: None,
        model: None,
        reasoning_effort: None,
        permission_mode: None,
        network_policy: Default::default(),
        attachment_revision: 0,
        root_attachments: Vec::new(),
        memory_incognito: false,
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    let accepted = match store
        .accept_turn(turn_id, chat.id, "fake", "question")
        .await
        .unwrap()
    {
        crate::storage::AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance: {outcome:?}"),
    };
    let lease_token = uuid::Uuid::new_v4();
    store
        .claim_turn(
            lease_token,
            accepted.available_at,
            accepted.available_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap()
        .turn
        .expect("accepted turn is claimable");
    let agent = Agent::new(
        Arc::new(RefusalProvider(events)),
        Arc::new(ToolRegistry::new()),
        store,
        AgentConfig {
            model: "fake".into(),
            ..Default::default()
        },
    )
    .with_durable_steer(lease_token);
    let (tx, rx) = unbounded();
    let outcome = agent
        .run_claimed_turn(&chat, turn_id, MessageId::new(), 1, &tx)
        .await
        .unwrap();
    drop(tx);
    let journaled = rx
        .filter_map(|item| async move {
            match item {
                ClaimedAgentEvent::Pending { event, .. } => Some(event),
                _ => None,
            }
        })
        .collect::<Vec<_>>()
        .await;
    (outcome, journaled)
}

#[tokio::test]
async fn foreground_refusal_distinguishes_empty_partial_and_bare_events() {
    let (empty, empty_events) = run_claimed_refusal(vec![ProviderEvent::Refusal {
        details: RefusalDetails::from_category(Some("cyber")),
    }])
    .await;
    let AgentTurnOutcome::Completed {
        output,
        stop_reason: StopReason::Refusal,
        refusal: Some(refusal),
        ..
    } = empty
    else {
        panic!("structured empty refusal should complete as refused");
    };
    assert_eq!(output.content, "");
    assert_eq!(refusal.category(), Some("cyber"));
    assert!(!refusal.partial_output());
    assert!(
        !empty_events
            .iter()
            .any(|e| matches!(e, AgentEvent::StreamInterrupted)),
        "a refusal with no started tool calls has nothing to discard"
    );

    let (partial, _) = run_claimed_refusal(vec![
        ProviderEvent::TextDelta {
            text: "A partial answer".into(),
        },
        ProviderEvent::Refusal {
            details: RefusalDetails::from_category(Some("general_harms")),
        },
    ])
    .await;
    let AgentTurnOutcome::Completed {
        output,
        stop_reason: StopReason::Refusal,
        refusal: Some(refusal),
        ..
    } = partial
    else {
        panic!("structured mid-stream refusal should complete as refused");
    };
    assert_eq!(output.content, "A partial answer");
    assert_eq!(refusal.category(), Some("general_harms"));
    assert!(refusal.partial_output());

    let (bare, _) = run_claimed_refusal(vec![ProviderEvent::Stop {
        reason: StopReason::Refusal,
    }])
    .await;
    let AgentTurnOutcome::Completed {
        output,
        stop_reason: StopReason::Refusal,
        refusal: Some(refusal),
        ..
    } = bare
    else {
        panic!("bare refusal stop should use default metadata");
    };
    assert_eq!(output.content, "");
    assert_eq!(refusal.category(), None);
    assert!(!refusal.partial_output());

    // Calls that started before the refusal were already journaled, so the
    // refusal has to mark them discarded or replay is left holding a call
    // that never resolves.
    let (with_calls, call_events) = run_claimed_refusal(vec![
        ProviderEvent::ToolCallStarted {
            index: 0,
            id: "call-0".into(),
            name: "echo".into(),
        },
        ProviderEvent::ToolCallArgsDelta {
            index: 0,
            fragment: "{\"text\"".into(),
        },
        ProviderEvent::Refusal {
            details: RefusalDetails::from_category(Some("cyber")),
        },
    ])
    .await;
    assert!(
        matches!(
            with_calls,
            AgentTurnOutcome::Completed {
                stop_reason: StopReason::Refusal,
                ..
            }
        ),
        "a refusal mid tool call still completes as refused"
    );
    let started = call_events
        .iter()
        .position(|e| matches!(e, AgentEvent::ToolCallStarted { .. }));
    let interrupted = call_events
        .iter()
        .position(|e| matches!(e, AgentEvent::StreamInterrupted));
    assert!(
        matches!((started, interrupted), (Some(a), Some(b)) if a < b),
        "the started call is marked discarded by the refusal"
    );
}

/// The in-process driver must not report success for a turn whose final
/// model response has neither text nor a tool call: the caller gets a
/// blank turn with nothing to act on and no error to explain it. The
/// worker refuses the same response (its disposition is to retry while
/// budgets allow); the in-process driver has no attempt accounting, so
/// the turn fails instead of completing.
#[tokio::test]
async fn an_empty_model_response_does_not_complete_an_in_process_turn() {
    struct EmptyProvider;

    #[async_trait]
    impl ModelProvider for EmptyProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("empty")
        }

        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            Ok(stream::iter(vec![ProviderEvent::Stop {
                reason: StopReason::EndTurn,
            }])
            .boxed())
        }
    }

    let db = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect_test_sqlite_fixture(&format!(
            "sqlite://{}?mode=rwc",
            db.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let chat = Chat {
        id: SessionId::new(),
        project_id: None,
        title: None,
        model: None,
        reasoning_effort: None,
        permission_mode: None,
        network_policy: Default::default(),
        attachment_revision: 0,
        root_attachments: Vec::new(),
        memory_incognito: false,
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();
    let agent = Agent::new(
        Arc::new(EmptyProvider),
        Arc::new(ToolRegistry::new()),
        store,
        AgentConfig {
            model: "fake".into(),
            ..Default::default()
        },
    );

    let (tx, rx) = unbounded();
    let result = agent.run_turn(&chat, "say something", &tx).await;
    drop(tx);
    let events: Vec<AgentEvent> = rx.collect().await;

    assert!(result.is_err());
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::TurnCompleted { .. })),
        "an empty response must not complete the turn"
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::TurnFailed { .. })),
        "the failure surfaces as TurnFailed"
    );
}

/// A mid-stream provider failure must keep the classification the
/// equivalent HTTP-status failure would have had: an in-band overload
/// surfaces to the client as `overloaded`, not the generic `provider`.
#[tokio::test]
async fn a_mid_stream_failure_reaches_the_client_with_its_classification() {
    struct OverloadedProvider;

    #[async_trait]
    impl ModelProvider for OverloadedProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("overloaded")
        }

        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            Ok(stream::iter(vec![
                ProviderEvent::TextDelta {
                    text: "partial".into(),
                },
                ProviderEvent::Failed {
                    error: ProviderErrorInfo::from_error(&AgentError::Overloaded(
                        "anthropic returned 500 (overloaded_error)".into(),
                    )),
                },
            ])
            .boxed())
        }
    }

    let db = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect_test_sqlite_fixture(&format!(
            "sqlite://{}?mode=rwc",
            db.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let chat = Chat {
        id: SessionId::new(),
        project_id: None,
        title: None,
        model: None,
        reasoning_effort: None,
        permission_mode: None,
        network_policy: Default::default(),
        attachment_revision: 0,
        root_attachments: Vec::new(),
        memory_incognito: false,
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();
    let agent = Agent::new(
        Arc::new(OverloadedProvider),
        Arc::new(ToolRegistry::new()),
        store,
        AgentConfig {
            model: "fake".into(),
            ..Default::default()
        },
    );

    let (tx, rx) = unbounded();
    let result = agent.run_turn(&chat, "say something", &tx).await;
    drop(tx);
    let events: Vec<AgentEvent> = rx.collect().await;

    assert_eq!(
        result.unwrap_err().kind(),
        "overloaded",
        "the turn fails under the classified kind"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::TurnFailed { error } if error.kind == "overloaded"
        )),
        "the classification reaches the client on TurnFailed"
    );
}

#[tokio::test]
async fn a_mid_stream_context_overflow_restarts_after_discarding_the_candidate() {
    struct OverflowThenAnswer {
        requests: Arc<Mutex<Vec<ChatRequest>>>,
    }

    #[async_trait]
    impl ModelProvider for OverflowThenAnswer {
        fn id(&self) -> ProviderId {
            ProviderId::new("overflow-then-answer")
        }

        async fn stream(&self, request: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            let mut requests = self.requests.lock().unwrap();
            requests.push(request);
            let first = requests.len() == 1;
            drop(requests);

            let events = if first {
                vec![
                    ProviderEvent::TextDelta {
                        text: "discard me".into(),
                    },
                    ProviderEvent::ToolCallStarted {
                        index: 0,
                        id: "partial-call".into(),
                        name: "missing_tool".into(),
                    },
                    ProviderEvent::ToolCallArgsDelta {
                        index: 0,
                        fragment: "{\"unfinished\":".into(),
                    },
                    ProviderEvent::Usage(Usage {
                        input_tokens: 11,
                        output_tokens: 3,
                        ..Usage::default()
                    }),
                    ProviderEvent::Failed {
                        error: ProviderErrorInfo::from_error(&AgentError::PromptTooLong(
                            "context overflow".into(),
                        )),
                    },
                ]
            } else {
                vec![
                    ProviderEvent::TextDelta {
                        text: "recovered".into(),
                    },
                    ProviderEvent::Usage(Usage {
                        input_tokens: 7,
                        output_tokens: 2,
                        ..Usage::default()
                    }),
                    ProviderEvent::Stop {
                        reason: StopReason::EndTurn,
                    },
                ]
            };
            Ok(stream::iter(events).boxed())
        }
    }

    let (store, chat, _workspace) = cancel_test_chat().await;
    let requests = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        Arc::new(OverflowThenAnswer {
            requests: requests.clone(),
        }),
        Arc::new(ToolRegistry::new()),
        store.clone(),
        AgentConfig {
            model: "fake".into(),
            context_window: 64,
            ..Default::default()
        },
    );

    let (tx, rx) = unbounded();
    agent
        .run_turn(&chat, &"word ".repeat(200), &tx)
        .await
        .unwrap();
    drop(tx);
    let events: Vec<AgentEvent> = rx.collect().await;

    let request_tokens = {
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2, "the same model step is retried once");
        [
            context::estimate_transcript_tokens(&requests[0].messages),
            context::estimate_transcript_tokens(&requests[1].messages),
        ]
    };
    assert!(
        request_tokens[1] < request_tokens[0],
        "the retry uses the next reduction level"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, AgentEvent::StreamInterrupted))
            .count(),
        1,
        "clients clear the abandoned prose and tool call before the retry"
    );
    assert!(events
        .iter()
        .any(|event| matches!(event, AgentEvent::TextDelta { text } if text == "recovered")));
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::TurnCompleted {
                usage: Usage {
                    input_tokens: 18,
                    output_tokens: 5,
                    ..
                },
                ..
            }
        )),
        "usage includes provider work from the discarded attempt"
    );
    assert_eq!(
        store
            .list_messages(chat.id)
            .await
            .unwrap()
            .last()
            .unwrap()
            .content,
        "recovered",
        "only the successful candidate is persisted"
    );
}

#[tokio::test]
async fn a_stolen_lease_fences_intermediate_tool_effects() {
    let db = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect_test_sqlite_fixture(&format!(
            "sqlite://{}?mode=rwc",
            db.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let chat = Chat {
        id: SessionId::new(),
        project_id: None,
        title: None,
        model: None,
        reasoning_effort: None,
        permission_mode: None,
        network_policy: Default::default(),
        attachment_revision: 0,
        root_attachments: Vec::new(),
        memory_incognito: false,
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    store
        .accept_turn(turn_id, chat.id, "fake", "go")
        .await
        .unwrap();
    let now = Utc::now();
    let lease_token = uuid::Uuid::new_v4();
    store
        .claim_turn(lease_token, now, now + chrono::Duration::minutes(1))
        .await
        .unwrap();

    let ran = Arc::new(AtomicUsize::new(0));
    let tools = Arc::new(ToolRegistry::new().with(Box::new(SpyTool { ran: ran.clone() })));
    let agent = Agent::new(
        Arc::new(LeaseStealingProvider {
            store: store.clone(),
            // The steal reads a claim time past the lease expiry, so the
            // scan reclaims and terminalizes the turn deterministically.
            steal_at: now + chrono::Duration::minutes(2),
            stole: AtomicUsize::new(0),
        }),
        tools,
        store.clone(),
        AgentConfig {
            model: "fake".into(),
            ..Default::default()
        },
    )
    .with_durable_steer(lease_token);

    let (tx, rx) = unbounded();
    let outcome = agent
        .run_claimed_turn(&chat, turn_id, MessageId::new(), 1, &tx)
        .await
        .unwrap();
    drop(tx);
    let _ = rx.collect::<Vec<_>>().await;

    // The stale segment refuses to persist tool-call rows or run the tool.
    assert!(
        matches!(outcome, AgentTurnOutcome::Failed { .. }),
        "a stolen lease must not complete the turn: {outcome:?}"
    );
    assert_eq!(
        ran.load(Ordering::SeqCst),
        0,
        "a stolen lease must not execute tool side effects"
    );
    // The retry claim stands; the stale worker committed nothing.
    let turn = store.get_turn(turn_id).await.unwrap().unwrap();
    assert_eq!(turn.status, TurnRunStatus::Running);
    assert_ne!(turn.lease_token, Some(lease_token));
}

#[tokio::test]
async fn retry_abandons_an_inherited_pending_tool_without_replaying_it() {
    let db = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect_test_sqlite_fixture(&format!(
            "sqlite://{}?mode=rwc",
            db.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let chat = Chat {
        id: SessionId::new(),
        project_id: None,
        title: None,
        model: None,
        reasoning_effort: None,
        permission_mode: None,
        network_policy: Default::default(),
        attachment_revision: 0,
        root_attachments: Vec::new(),
        memory_incognito: false,
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    let accepted = match store
        .accept_turn(turn_id, chat.id, "fake", "go")
        .await
        .unwrap()
    {
        crate::storage::AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance: {outcome:?}"),
    };
    let first_claim_at = accepted.available_at;
    let first_lease = uuid::Uuid::new_v4();
    store
        .claim_turn(
            first_lease,
            first_claim_at,
            first_claim_at + chrono::Duration::seconds(1),
        )
        .await
        .unwrap();
    let call_id = CallId::new();
    let call = ToolCallRecord {
        id: call_id,
        chat_id: chat.id,
        turn_id,
        provider_id: "call_spy".into(),
        name: "spy".into(),
        arguments: serde_json::json!({}),
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

    // Simulate a crash after acceptance and possible execution but before
    // result commit. Reclaiming creates the next failure attempt.
    let retry_at = first_claim_at + chrono::Duration::seconds(2);
    let retry_lease = uuid::Uuid::new_v4();
    let retried = store
        .claim_turn(
            retry_lease,
            retry_at,
            retry_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap()
        .turn
        .unwrap();
    assert_eq!(retried.attempt_count, 2);

    let ran = Arc::new(AtomicUsize::new(0));
    let tools = Arc::new(ToolRegistry::new().with(Box::new(SpyTool { ran: ran.clone() })));
    let agent = Agent::new(
        Arc::new(AnswerOnlyProvider),
        tools,
        store.clone(),
        AgentConfig {
            model: "fake".into(),
            ..Default::default()
        },
    )
    .with_durable_steer(retry_lease);
    let (tx, rx) = unbounded();
    let outcome = agent
        .run_claimed_turn(&chat, turn_id, MessageId::new(), 1, &tx)
        .await
        .unwrap();
    drop(tx);
    let _ = rx.collect::<Vec<_>>().await;

    assert!(matches!(outcome, AgentTurnOutcome::Completed { .. }));
    assert_eq!(ran.load(Ordering::SeqCst), 0, "pending work was replayed");
    let stored = store
        .list_tool_calls(chat.id)
        .await
        .unwrap()
        .into_iter()
        .find(|item| item.id == call_id)
        .unwrap();
    assert_eq!(stored.status, ToolCallStatus::Failed);
    assert_eq!(
        stored.error_code.as_deref(),
        Some("tool_execution_interrupted")
    );
}

#[tokio::test]
async fn parallel_read_results_stay_ordered_even_when_a_failure_finishes_first() {
    struct SlowRead {
        started: Arc<tokio::sync::Notify>,
        release: Mutex<Option<oneshot::Receiver<()>>>,
    }

    #[async_trait]
    impl Tool for SlowRead {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "slow_read".into(),
                description: "a deliberately delayed read".into(),
                input_schema: serde_json::json!({"type": "object"}),
            }
        }

        fn approval_class(&self) -> ApprovalClass {
            ApprovalClass::ReadOnly
        }

        async fn execute(&self, _ctx: &ToolCtx, _args: Value) -> Result<ToolOutput> {
            self.started.notify_one();
            let release = self
                .release
                .lock()
                .unwrap()
                .take()
                .expect("slow read runs once");
            release.await.expect("test releases the slow read");
            Ok(ToolOutput::text("slow result"))
        }
    }

    struct FastFailingRead;

    #[async_trait]
    impl Tool for FastFailingRead {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "fast_read".into(),
                description: "a read that fails immediately".into(),
                input_schema: serde_json::json!({"type": "object"}),
            }
        }

        fn approval_class(&self) -> ApprovalClass {
            ApprovalClass::ReadOnly
        }

        async fn execute(&self, _ctx: &ToolCtx, _args: Value) -> Result<ToolOutput> {
            Ok(ToolOutput::error("fast read failed"))
        }
    }

    struct ParallelReadProvider {
        calls: AtomicUsize,
        received_results: Arc<Mutex<Vec<(String, String, bool)>>>,
    }

    #[async_trait]
    impl ModelProvider for ParallelReadProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("parallel-read")
        }

        async fn stream(&self, request: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            let events = if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                vec![
                    ProviderEvent::ToolCallStarted {
                        index: 0,
                        id: "slow_call".into(),
                        name: "slow_read".into(),
                    },
                    ProviderEvent::ToolCallArgsDelta {
                        index: 0,
                        fragment: "{}".into(),
                    },
                    ProviderEvent::ToolCallStarted {
                        index: 1,
                        id: "fast_call".into(),
                        name: "fast_read".into(),
                    },
                    ProviderEvent::ToolCallArgsDelta {
                        index: 1,
                        fragment: "{}".into(),
                    },
                    ProviderEvent::Stop {
                        reason: StopReason::ToolUse,
                    },
                ]
            } else {
                let results = request
                    .messages
                    .last()
                    .expect("the second request includes the tool results")
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            is_error,
                        } => Some((tool_use_id.clone(), content.clone(), *is_error)),
                        _ => None,
                    })
                    .collect();
                *self.received_results.lock().unwrap() = results;
                vec![
                    ProviderEvent::TextDelta {
                        text: "done".into(),
                    },
                    ProviderEvent::Stop {
                        reason: StopReason::EndTurn,
                    },
                ]
            };
            Ok(stream::iter(events).boxed())
        }
    }

    let (store, chat, _workspace) = cancel_test_chat().await;
    let slow_started = Arc::new(tokio::sync::Notify::new());
    let (release_slow, slow_release) = oneshot::channel();
    let received_results = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        Arc::new(ParallelReadProvider {
            calls: AtomicUsize::new(0),
            received_results: received_results.clone(),
        }),
        Arc::new(
            ToolRegistry::new()
                .with(Box::new(SlowRead {
                    started: slow_started.clone(),
                    release: Mutex::new(Some(slow_release)),
                }))
                .with(Box::new(FastFailingRead)),
        ),
        store.clone(),
        AgentConfig {
            model: "parallel-read".into(),
            ..Default::default()
        },
    );

    let chat_id = chat.id;
    let (tx, mut rx) = unbounded();
    let turn = tokio::spawn(async move { agent.run_turn(&chat, "go", &tx).await });
    slow_started.notified().await;
    let first_completion = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            if let Some(AgentEvent::ToolCallCompleted { output, .. }) = rx.next().await {
                break output;
            }
        }
    })
    .await
    .expect("the fast call must finish before the slow call is released");
    assert!(first_completion.is_error);
    assert_eq!(first_completion.content, "fast read failed");
    release_slow.send(()).unwrap();

    tokio::time::timeout(std::time::Duration::from_secs(1), turn)
        .await
        .expect("the released read finishes the turn")
        .unwrap()
        .unwrap();
    assert_eq!(
        *received_results.lock().unwrap(),
        vec![
            ("slow_call".into(), "slow result".into(), false),
            ("fast_call".into(), "fast read failed".into(), true),
        ],
        "the next model request keeps the provider's requested order"
    );
    assert!(
        store
            .list_tool_calls(chat_id)
            .await
            .unwrap()
            .iter()
            .all(|call| call.status.is_terminal()),
        "a failed sibling cannot leave the slow call pending"
    );
}
