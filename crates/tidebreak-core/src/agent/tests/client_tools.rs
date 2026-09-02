use super::*;

#[tokio::test]
async fn claimed_agent_returns_a_client_tool_checkpoint_without_executing_it() {
    let db = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            db.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let chat = Chat {
        id: ChatId::new(),
        project_id: None,
        title: None,
        model: None,
        reasoning_effort: None,
        permission_mode: None,
        network_policy: Default::default(),
        attachment_revision: 0,
        root_attachments: Vec::new(),
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    store
        .accept_turn(turn_id, chat.id, "fake", "connect documents")
        .await
        .unwrap();
    let lease_token = uuid::Uuid::new_v4();
    let claimed_at = Utc::now();
    store
        .claim_turn(
            lease_token,
            claimed_at,
            claimed_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap();
    let client_spec = ToolSpec {
        name: "connect_folder".into(),
        description: "Ask the desktop to connect a folder".into(),
        input_schema: serde_json::json!({"type": "object"}),
    };
    let mut registry = ToolRegistry::new();
    registry.register_client(client_spec.clone(), ApprovalClass::ReadOnly);
    assert_eq!(
        registry.execution("connect_folder"),
        Some(ToolCallExecution::Client)
    );
    assert!(registry.get("connect_folder").is_none());
    assert_eq!(registry.specs(), vec![client_spec]);
    let agent = Agent::new(
        Arc::new(ClientToolProvider {
            assistant_text: false,
            sibling_call: false,
            name: "connect_folder",
            arguments: r#"{"hint":"Documents"}"#,
        }),
        Arc::new(registry),
        store.clone(),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    )
    .with_durable_steer(lease_token)
    .with_foreground_agent_orchestration();
    let (tx, rx) = unbounded();
    let outcome = agent
        .run_claimed_turn(&chat, turn_id, MessageId::new(), 1, &tx)
        .await
        .unwrap();
    drop(tx);
    let events = emitted_events(rx.collect().await);
    let AgentTurnOutcome::ClientToolCall {
        request,
        usage,
        steer_revision,
        model_steps,
    } = outcome
    else {
        panic!("claimed agent should return a client checkpoint");
    };
    assert_eq!(request.chat_id, chat.id);
    assert_eq!(request.turn_id, turn_id);
    assert_eq!(request.provider_id, "native_1");
    assert_eq!(request.name, "connect_folder");
    assert_eq!(request.arguments, serde_json::json!({"hint": "Documents"}));
    assert_eq!(usage.input_tokens, 5);
    assert_eq!(usage.output_tokens, 2);
    assert_eq!(steer_revision, 0);
    assert_eq!(model_steps, 1);
    assert!(store.list_tool_calls(chat.id).await.unwrap().is_empty());
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ToolCallStarted { name, .. } if name == "connect_folder"
    )));

    let mut validated_registry = ToolRegistry::new();
    validated_registry.register_validated_client(
        crate::request_folder_access_tool_spec(),
        ApprovalClass::ReadOnly,
        crate::validate_request_folder_access_arguments,
    );
    let invalid_agent = Agent::new(
            Arc::new(ClientToolProvider {
                assistant_text: false,
                sibling_call: false,
                name: crate::REQUEST_FOLDER_ACCESS_TOOL,
                arguments: r#"{"reason":"Read reports","requested_capabilities":["write_files"],"path":"/Users/example/Documents"}"#,
            }),
            Arc::new(validated_registry),
            store.clone(),
            AgentConfig {
                model: "fake".into(),
                max_steps: 1,
                ..AgentConfig::default()
            },
        )
        .with_durable_steer(lease_token);
    let (invalid_tx, _invalid_rx) = unbounded();
    let invalid_outcome = invalid_agent
        .run_claimed_turn(&chat, turn_id, MessageId::new(), 1, &invalid_tx)
        .await
        .unwrap();
    // Arguments the validator rejects never become a request: the call is
    // answered in place and the turn runs on rather than suspending on it.
    assert!(
        !matches!(invalid_outcome, AgentTurnOutcome::ClientToolCall { .. }),
        "invalid arguments must not reach a checkpoint: {invalid_outcome:?}"
    );
    assert!(store.list_tool_calls(chat.id).await.unwrap().is_empty());
}

#[tokio::test]
async fn user_questions_are_advertised_and_executable_only_in_the_foreground() {
    let mut registry = ToolRegistry::new();
    registry.register_validated_foreground_client(
        crate::ask_user_questions_tool_spec(),
        ApprovalClass::ReadOnly,
        crate::validate_ask_user_questions_arguments,
    );

    assert!(registry.specs().is_empty());
    assert_eq!(
        registry
            .specs_for_foreground(true)
            .into_iter()
            .map(|spec| spec.name)
            .collect::<Vec<_>>(),
        vec![crate::ASK_USER_QUESTIONS_TOOL]
    );
    assert_eq!(
        registry.execution(crate::ASK_USER_QUESTIONS_TOOL),
        Some(ToolCallExecution::Client)
    );
    assert!(registry.is_foreground_client(crate::ASK_USER_QUESTIONS_TOOL));
    assert!(registry.client_arguments_are_valid(
        crate::ASK_USER_QUESTIONS_TOOL,
        &serde_json::json!({
            "questions": [{
                "id": "target",
                "header": "Target",
                "question": "Where should I deploy?",
                "options": [{
                    "id": "staging",
                    "label": "Staging",
                    "description": "Deploy for verification."
                }]
            }]
        })
    ));
    assert!(!registry.client_arguments_are_valid(
        crate::ASK_USER_QUESTIONS_TOOL,
        &serde_json::json!({"questions": []})
    ));

    let db = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            db.path().join("foreground-question.db").display()
        ))
        .await
        .unwrap(),
    );
    let chat = Chat {
        id: ChatId::new(),
        project_id: None,
        title: None,
        model: None,
        reasoning_effort: None,
        permission_mode: None,
        network_policy: Default::default(),
        attachment_revision: 0,
        root_attachments: Vec::new(),
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();
    let agent = Agent::new(
        Arc::new(ClientToolProvider {
            assistant_text: false,
            sibling_call: false,
            name: crate::ASK_USER_QUESTIONS_TOOL,
            arguments: r#"{"questions":[{"id":"target","header":"Target","question":"Where should I deploy?","options":[{"id":"staging","label":"Staging","description":"Deploy for verification."}]}]}"#,
        }),
        Arc::new(registry),
        store.clone(),
        AgentConfig {
            model: "fake".into(),
            max_steps: 1,
            ..AgentConfig::default()
        },
    );
    let (tx, rx) = unbounded();
    agent.run_turn(&chat, "deploy", &tx).await.unwrap();
    drop(tx);
    let events = rx.collect::<Vec<_>>().await;
    assert!(!events
        .iter()
        .any(|event| matches!(event, AgentEvent::UserQuestionsAsked { .. })));
    assert!(store.list_tool_calls(chat.id).await.unwrap().is_empty());
}

#[tokio::test]
async fn claimed_foreground_agent_returns_exact_ordered_wait_checkpoint() {
    let db = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            db.path().join("wait.db").display()
        ))
        .await
        .unwrap(),
    );
    let chat = Chat {
        id: ChatId::new(),
        project_id: None,
        title: None,
        model: None,
        reasoning_effort: None,
        permission_mode: None,
        network_policy: Default::default(),
        attachment_revision: 0,
        root_attachments: Vec::new(),
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    store
        .accept_turn(turn_id, chat.id, "fake", "wait for both")
        .await
        .unwrap();
    let lease_token = uuid::Uuid::new_v4();
    let claimed_at = Utc::now();
    store
        .claim_turn(
            lease_token,
            claimed_at,
            claimed_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap();
    let mut registry = ToolRegistry::new();
    registry.register_foreground_agent_orchestration();
    let arguments = r#"{"agent_ids":["00000000-0000-0000-0000-000000000002","00000000-0000-0000-0000-000000000001"]}"#;
    let agent = Agent::new(
        Arc::new(ClientToolProvider {
            assistant_text: false,
            sibling_call: false,
            name: crate::WAIT_FOR_AGENTS_TOOL,
            arguments,
        }),
        Arc::new(registry),
        store,
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    )
    .with_durable_steer(lease_token)
    .with_foreground_agent_orchestration();
    let (tx, _rx) = unbounded();
    let outcome = agent
        .run_claimed_turn(&chat, turn_id, MessageId::new(), 1, &tx)
        .await
        .unwrap();
    let AgentTurnOutcome::WaitForAgents {
        request,
        steer_revision,
        model_steps,
        ..
    } = outcome
    else {
        panic!("foreground agent should return an ordered wait checkpoint");
    };
    assert_eq!(request.provider_id, "native_1");
    assert_eq!(
        request.arguments,
        serde_json::from_str::<Value>(arguments).unwrap()
    );
    assert_eq!(
        request.child_run_ids,
        [
            "00000000-0000-0000-0000-000000000002",
            "00000000-0000-0000-0000-000000000001",
        ]
        .map(|id| AgentRunId(uuid::Uuid::parse_str(id).unwrap()))
    );
    assert!(request.is_well_formed());
    assert_eq!(steer_revision, 0);
    assert_eq!(model_steps, 1);
}

#[tokio::test]
async fn a_mixed_batch_runs_the_server_call_then_checkpoints_the_client_one() {
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("a.txt"), "sibling result").unwrap();
    let db = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            db.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let chat = Chat {
        id: ChatId::new(),
        project_id: None,
        title: None,
        model: None,
        reasoning_effort: None,
        permission_mode: None,
        network_policy: Default::default(),
        attachment_revision: 0,
        root_attachments: Vec::new(),
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    store
        .accept_turn(turn_id, chat.id, "fake", "connect documents")
        .await
        .unwrap();
    let lease_token = uuid::Uuid::new_v4();
    let claimed_at = Utc::now();
    store
        .claim_turn(
            lease_token,
            claimed_at,
            claimed_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap();
    let mut registry = ToolRegistry::new();
    registry.register_client(
        ToolSpec {
            name: "connect_folder".into(),
            description: "Ask the desktop to connect a folder".into(),
            input_schema: serde_json::json!({"type": "object"}),
        },
        ApprovalClass::ReadOnly,
    );
    registry.register(Box::new(ReadFile));
    let agent = Agent::new(
        Arc::new(ClientToolProvider {
            assistant_text: true,
            sibling_call: true,
            name: "connect_folder",
            arguments: r#"{"hint":"Documents"}"#,
        }),
        Arc::new(registry),
        store.clone(),
        AgentConfig {
            model: "fake".into(),
            max_steps: 2,
            tool_scratch: Some(tool_scratch(workspace.path())),
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

    // This batch used to be refused twice and then fail the turn, throwing
    // away the preamble and the sibling's finished work each time. Now the
    // server call runs and commits, and the client call still leaves as the
    // step's checkpoint — a checkpoint that carries exactly one call.
    let AgentTurnOutcome::ClientToolCall {
        request,
        model_steps,
        ..
    } = outcome
    else {
        panic!("the client call should still reach its checkpoint: {outcome:?}");
    };
    assert_eq!(request.name, "connect_folder");
    assert_eq!(model_steps, 1);
    assert!(!events
        .iter()
        .any(|event| matches!(event, AgentEvent::StreamInterrupted)));
    let messages = store.list_messages(chat.id).await.unwrap();
    assert!(
        messages
            .iter()
            .any(|message| message.role == Role::Assistant
                && message.content.contains("I will connect it")),
        "the preamble should survive the checkpoint: {messages:?}"
    );
    // The sibling is terminal before the turn suspends, so the resuming
    // attempt finds nothing pending to guess about.
    let calls = store.list_tool_calls(chat.id).await.unwrap();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert_eq!(calls[0].name, "read_file");
    assert_eq!(calls[0].status, ToolCallStatus::Completed);
    assert_eq!(calls[0].result.as_deref(), Some("sibling result"));
}

/// Arguments the loop cannot parse used to discard the step and count
/// towards failing the turn. They are a property of the one call, so they
/// are answered like any other bad call: the model is told what was wrong
/// and keeps the step it already spent.
#[tokio::test]
async fn a_client_call_with_unparseable_arguments_is_answered_not_discarded() {
    let db = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            db.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let chat = Chat {
        id: ChatId::new(),
        project_id: None,
        title: None,
        model: None,
        reasoning_effort: None,
        permission_mode: None,
        network_policy: Default::default(),
        attachment_revision: 0,
        root_attachments: Vec::new(),
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    store
        .accept_turn(turn_id, chat.id, "fake", "connect documents")
        .await
        .unwrap();
    let lease_token = uuid::Uuid::new_v4();
    let claimed_at = Utc::now();
    store
        .claim_turn(
            lease_token,
            claimed_at,
            claimed_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap();
    let mut registry = ToolRegistry::new();
    registry.register_client(
        ToolSpec {
            name: "connect_folder".into(),
            description: "Ask the desktop to connect a folder".into(),
            input_schema: serde_json::json!({"type": "object"}),
        },
        ApprovalClass::ReadOnly,
    );
    let agent = Agent::new(
        Arc::new(ClientToolProvider {
            assistant_text: false,
            sibling_call: false,
            name: "connect_folder",
            arguments: "{not json",
        }),
        Arc::new(registry),
        store.clone(),
        AgentConfig {
            model: "fake".into(),
            max_steps: 1,
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

    // Nothing to check point on: the call never became a request, so the
    // turn runs out its steps rather than suspending on a malformed one.
    assert!(
        !matches!(outcome, AgentTurnOutcome::ClientToolCall { .. }),
        "a call that could not be parsed must not reach a checkpoint: {outcome:?}"
    );
    assert!(!events
        .iter()
        .any(|event| matches!(event, AgentEvent::StreamInterrupted)));
    let completions: Vec<&ToolOutput> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolCallCompleted { output, .. } => Some(output),
            _ => None,
        })
        .collect();
    assert_eq!(completions.len(), 1, "{completions:?}");
    assert!(completions[0].is_error);
    assert!(
        completions[0].content.contains("not valid JSON"),
        "the model should be told what to fix: {completions:?}"
    );
    // Declined before it ran, so there is no record for a resume to find.
    assert!(store.list_tool_calls(chat.id).await.unwrap().is_empty());
}

/// The model narrates before it acts. Rejecting a client call for carrying
/// a preamble spent the whole step budget on a correction the model never
/// satisfied — the same failure #372 fixed for sensitive calls. The step
/// must check point instead, keeping the preamble durable across the
/// resume.
#[tokio::test]
async fn client_call_with_prose_checkpoints_and_keeps_the_preamble() {
    let db = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            db.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let chat = Chat {
        id: ChatId::new(),
        project_id: None,
        title: None,
        model: None,
        reasoning_effort: None,
        permission_mode: None,
        network_policy: Default::default(),
        attachment_revision: 0,
        root_attachments: Vec::new(),
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    store
        .accept_turn(turn_id, chat.id, "fake", "connect documents")
        .await
        .unwrap();
    let lease_token = uuid::Uuid::new_v4();
    let claimed_at = Utc::now();
    store
        .claim_turn(
            lease_token,
            claimed_at,
            claimed_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap();
    let mut registry = ToolRegistry::new();
    registry.register_client(
        ToolSpec {
            name: "connect_folder".into(),
            description: "Ask the desktop to connect a folder".into(),
            input_schema: serde_json::json!({"type": "object"}),
        },
        ApprovalClass::ReadOnly,
    );
    let agent = Agent::new(
        Arc::new(ClientToolProvider {
            assistant_text: true,
            sibling_call: false,
            name: "connect_folder",
            arguments: r#"{"hint":"Documents"}"#,
        }),
        Arc::new(registry),
        store.clone(),
        AgentConfig {
            model: "fake".into(),
            max_steps: 2,
            ..AgentConfig::default()
        },
    )
    .with_durable_steer(lease_token);
    let (tx, _rx) = unbounded();
    let outcome = agent
        .run_claimed_turn(&chat, turn_id, MessageId::new(), 1, &tx)
        .await
        .unwrap();

    // One step, not an exhausted budget: the call reached its checkpoint.
    let AgentTurnOutcome::ClientToolCall {
        request,
        model_steps,
        ..
    } = outcome
    else {
        panic!("expected a client tool checkpoint, got {outcome:?}");
    };
    assert_eq!(request.name, "connect_folder");
    assert_eq!(model_steps, 1);

    // The preamble is durable, so the resumed attempt rebuilds it.
    let messages = store.list_messages(chat.id).await.unwrap();
    assert!(
        messages
            .iter()
            .any(|message| message.role == Role::Assistant
                && message.content.contains("I will connect it")),
        "the assistant preamble should survive the checkpoint: {messages:?}"
    );
}
