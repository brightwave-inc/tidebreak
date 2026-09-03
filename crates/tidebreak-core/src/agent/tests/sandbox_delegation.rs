use super::*;

struct SandboxCorrectionProvider {
    calls: AtomicUsize,
}

struct SiblingSandboxSpawnProvider;

#[async_trait]
impl ModelProvider for SandboxCorrectionProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("sandbox-correction")
    }

    async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        let first = self.calls.fetch_add(1, Ordering::SeqCst) == 0;
        let arguments = if first {
            r#"{"task":"Research the error handling options.","resource":null}"#
        } else {
            r#"{"task":"Research the error handling options."}"#
        };
        Ok(stream::iter(vec![
            ProviderEvent::ToolCallStarted {
                index: 0,
                id: if first {
                    "sandbox_null".into()
                } else {
                    "sandbox_omitted".into()
                },
                name: crate::SPAWN_SANDBOX_AGENT_TOOL.into(),
            },
            ProviderEvent::ToolCallArgsDelta {
                index: 0,
                fragment: arguments.into(),
            },
            ProviderEvent::Usage(Usage {
                input_tokens: 5,
                output_tokens: 2,
                ..Usage::default()
            }),
            ProviderEvent::Stop {
                reason: StopReason::ToolUse,
            },
        ])
        .boxed())
    }
}

#[async_trait]
impl ModelProvider for SiblingSandboxSpawnProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("sibling-sandbox-spawn")
    }

    async fn stream(&self, req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        if req.tool_choice == Some(ToolChoice::None) || req.tools.is_empty() {
            return Ok(stream::iter(vec![
                ProviderEvent::TextDelta {
                    text: "I need to reissue each delegation separately.".into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ])
            .boxed());
        }
        Ok(stream::iter(vec![
            ProviderEvent::ToolCallStarted {
                index: 0,
                id: "spawn_a".into(),
                name: crate::SPAWN_SANDBOX_AGENT_TOOL.into(),
            },
            ProviderEvent::ToolCallArgsDelta {
                index: 0,
                fragment: r#"{"task":"research A"}"#.into(),
            },
            ProviderEvent::ToolCallStarted {
                index: 1,
                id: "spawn_b".into(),
                name: crate::SPAWN_SANDBOX_AGENT_TOOL.into(),
            },
            ProviderEvent::ToolCallArgsDelta {
                index: 1,
                fragment: r#"{"task":"research B"}"#.into(),
            },
            ProviderEvent::Stop {
                reason: StopReason::ToolUse,
            },
        ])
        .boxed())
    }
}

#[tokio::test]
async fn claimed_foreground_agent_returns_one_bounded_sandbox_checkpoint() {
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
        // Consent is not what this test is about: the chat has already
        // said it will not be asked, so the checkpoint shape is what
        // shows through.
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
        .accept_turn(turn_id, chat.id, "fake", "research this")
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
    assert!(registry.specs().is_empty());
    let advertised = registry
        .specs_for_foreground(true)
        .into_iter()
        .map(|spec| spec.name)
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(
        advertised,
        [crate::SPAWN_SANDBOX_AGENT_TOOL, crate::WAIT_FOR_AGENTS_TOOL]
            .into_iter()
            .map(str::to_owned)
            .collect()
    );
    let agent = Agent::new(
        Arc::new(ClientToolProvider {
            assistant_text: false,
            sibling_call: false,
            name: crate::SPAWN_SANDBOX_AGENT_TOOL,
            arguments: r#"{"task":"Research the error handling options."}"#,
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
    let AgentTurnOutcome::SandboxAgentSpawn {
        request,
        usage,
        steer_revision,
        model_steps,
        ..
    } = outcome
    else {
        panic!("foreground agent should return a sandbox checkpoint");
    };
    assert_eq!(request.task, "Research the error handling options.");
    assert_eq!(
        request.child_run_id,
        AgentRunId::sandbox_for_spawn_call(request.call_id)
    );
    assert!(request.is_well_formed());
    assert_eq!(usage.input_tokens, 5);
    assert_eq!(usage.output_tokens, 2);
    assert_eq!(steer_revision, 0);
    assert_eq!(model_steps, 1);
    assert!(store.list_tool_calls(chat.id).await.unwrap().is_empty());
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ToolCallStarted { name, .. }
            if name == crate::SPAWN_SANDBOX_AGENT_TOOL
    )));

    let mut correction_registry = ToolRegistry::new();
    correction_registry.register_foreground_agent_orchestration();
    let correction_provider = Arc::new(SandboxCorrectionProvider {
        calls: AtomicUsize::new(0),
    });
    let correction_agent = Agent::new(
        correction_provider.clone(),
        Arc::new(correction_registry),
        store.clone(),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    )
    .with_durable_steer(lease_token)
    .with_foreground_agent_orchestration();
    let (correction_tx, correction_rx) = unbounded();
    let corrected = correction_agent
        .run_claimed_turn(&chat, turn_id, MessageId::new(), 1, &correction_tx)
        .await
        .unwrap();
    drop(correction_tx);
    let correction_events = emitted_events(correction_rx.collect().await);
    let AgentTurnOutcome::SandboxAgentSpawn {
        request,
        model_steps,
        ..
    } = corrected
    else {
        panic!("foreground agent should correct a noncanonical sandbox resource");
    };
    assert_eq!(correction_provider.calls.load(Ordering::SeqCst), 2);
    assert_eq!(model_steps, 2);
    assert_eq!(
        request.arguments,
        serde_json::json!({"task": "Research the error handling options."})
    );
    assert!(request.is_well_formed());
    assert!(store.list_tool_calls(chat.id).await.unwrap().is_empty());
    // The correction arrives as the call's own result rather than as a
    // discarded step, so the assistant's output for that step survives it.
    assert!(!correction_events
        .iter()
        .any(|event| matches!(event, AgentEvent::StreamInterrupted)));
    assert!(
        correction_events.iter().any(|event| matches!(
            event,
            AgentEvent::ToolCallCompleted { output, .. }
                if output.is_error && output.content.contains("omit `resource`")
        )),
        "{correction_events:?}"
    );
}

#[tokio::test]
async fn sibling_sandbox_spawns_are_rejected_before_any_checkpoint() {
    let db = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect_test_sqlite_fixture(&format!(
            "sqlite://{}?mode=rwc",
            db.path().join("siblings.db").display()
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
        .accept_turn(turn_id, chat.id, "fake", "delegate")
        .await
        .unwrap();
    let lease_token = uuid::Uuid::new_v4();
    let now = Utc::now();
    store
        .claim_turn(lease_token, now, now + chrono::Duration::minutes(1))
        .await
        .unwrap();
    let mut registry = ToolRegistry::new();
    registry.register_foreground_agent_orchestration();
    let agent = Agent::new(
        Arc::new(SiblingSandboxSpawnProvider),
        Arc::new(registry),
        store.clone(),
        AgentConfig {
            model: "fake".into(),
            max_steps: 1,
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
    assert!(
        !matches!(outcome, AgentTurnOutcome::SandboxAgentSpawn { .. }),
        "multiple standalone spawn calls must not produce a checkpoint: {outcome:?}"
    );
    assert!(store.list_tool_calls(chat.id).await.unwrap().is_empty());
    let events = emitted_events(rx.collect().await);
    let corrections = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                AgentEvent::ToolCallCompleted { output, .. }
                    if output.is_error && output.content.contains("must be called alone")
            )
        })
        .count();
    assert_eq!(
        corrections, 2,
        "both spawn calls must be answered: {events:?}"
    );
}

#[tokio::test]
async fn report_blocked_returns_a_persisted_refused_outcome_with_the_explanation() {
    let db = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect_test_sqlite_fixture(&format!(
            "sqlite://{}?mode=rwc",
            db.path().join("report-blocked.db").display()
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
        .accept_turn(turn_id, chat.id, "fake", "produce the missing report")
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
        crate::agent_tools::report_blocked_tool_spec(),
        ApprovalClass::ReadOnly,
        crate::agent_tools::validate_report_blocked_arguments,
    );
    let explanation =
        "I cannot produce the requested report because its mandatory source is unavailable.";
    let agent = Agent::new(
        Arc::new(ClientToolProvider {
            assistant_text: false,
            sibling_call: false,
            name: crate::agent_tools::REPORT_BLOCKED_TOOL,
            arguments: r#"{"reason_code":"required_source_missing","explanation":"I cannot produce the requested report because its mandatory source is unavailable."}"#,
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
    let output_message_id = MessageId::new();
    let (tx, rx) = unbounded();
    let outcome = agent
        .run_claimed_turn(&chat, turn_id, output_message_id, 1, &tx)
        .await
        .unwrap();
    drop(tx);
    let AgentTurnOutcome::Completed {
        output,
        stop_reason,
        refusal,
        model_steps,
        ..
    } = outcome
    else {
        panic!("report_blocked must return the terminal refused outcome");
    };
    assert_eq!(output.id, output_message_id);
    assert_eq!(output.content, explanation);
    assert_eq!(stop_reason, StopReason::Refusal);
    assert_eq!(refusal.unwrap().category(), Some("blocked"));
    assert_eq!(model_steps, 1);

    let calls = store.list_tool_calls(chat.id).await.unwrap();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert_eq!(calls[0].name, crate::agent_tools::REPORT_BLOCKED_TOOL);
    assert_eq!(calls[0].execution, ToolCallExecution::Server);
    assert_eq!(calls[0].status, ToolCallStatus::Completed);
    assert_eq!(calls[0].result.as_deref(), Some(explanation));
    let events = emitted_events(rx.collect().await);
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ToolCallCompleted { output, .. }
            if !output.is_error && output.content == explanation
    )));
}

/// A background run's own calls never come back to this chat's gate, so
/// the spawn is the only moment consent can be asked for. A chat that
/// would park a foreground call parks the delegation too, and a refusal
/// leaves no checkpoint for the worker to admit a child from.
#[tokio::test]
async fn a_refused_delegation_never_reaches_a_spawn_checkpoint() {
    let (outcome, events) = drive_gated_delegation(Arc::new(RefuseGate)).await;
    assert!(
        !matches!(outcome, AgentTurnOutcome::SandboxAgentSpawn { .. }),
        "a refused delegation must not yield a spawn checkpoint: {outcome:?}"
    );
    // The card names the policy the child would inherit, because egress is
    // what the reader is deciding.
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::ApprovalRequired { kind, preview, .. }
                if *kind == ToolApprovalKind::DelegateMayRunBackgroundAgent
                    && matches!(
                        preview,
                        Some(ToolActionPreview::DelegateAgent { task, network })
                            if task == "research A"
                                && *network == crate::NetworkPolicy::Open
                    )
        )),
        "{events:?}"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::ToolCallCompleted { output, .. } if output.is_error
        )),
        "the model is told the delegation was refused: {events:?}"
    );
}

/// An approved delegation is admitted through the ordinary spawn
/// checkpoint, carrying the flag that tells it to finalize the row the
/// approval parked on rather than insert a second one.
#[tokio::test]
async fn an_approved_delegation_checkpoints_against_its_own_parked_call() {
    let (outcome, _events) = drive_gated_delegation(Arc::new(crate::AutoApproveGate)).await;
    let AgentTurnOutcome::SandboxAgentSpawn { request, .. } = outcome else {
        panic!("an approved delegation should produce a checkpoint: {outcome:?}");
    };
    assert_eq!(request.task, "research A");
    assert!(request.approval_gated);
}

/// Drive one delegation in a chat that asks before it acts, with the
/// claimed-turn sink drained concurrently so the gate's journal flush is
/// acknowledged the way the worker acknowledges it.
async fn drive_gated_delegation(
    gate: Arc<dyn ApprovalGate>,
) -> (AgentTurnOutcome, Vec<AgentEvent>) {
    let db = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect_test_sqlite_fixture(&format!(
            "sqlite://{}?mode=rwc",
            db.path().join("gate.db").display()
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
        network_policy: crate::NetworkPolicy::Open,
        attachment_revision: 0,
        root_attachments: Vec::new(),
        memory_incognito: false,
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    store
        .accept_turn(turn_id, chat.id, "fake", "delegate")
        .await
        .unwrap();
    let lease_token = uuid::Uuid::new_v4();
    let now = Utc::now();
    store
        .claim_turn(lease_token, now, now + chrono::Duration::minutes(1))
        .await
        .unwrap();
    let mut registry = ToolRegistry::new();
    registry.register_foreground_agent_orchestration();
    let agent = Agent::new(
        Arc::new(ClientToolProvider {
            assistant_text: false,
            sibling_call: false,
            name: crate::SPAWN_SANDBOX_AGENT_TOOL,
            arguments: r#"{"task":"research A"}"#,
        }),
        Arc::new(registry),
        store,
        AgentConfig {
            model: "fake".into(),
            max_steps: 1,
            ..AgentConfig::default()
        },
    )
    .with_durable_steer(lease_token)
    .with_approvals(gate)
    .with_foreground_agent_orchestration();
    let (tx, mut rx) = unbounded();
    let chat_for_turn = chat.clone();
    let handle = tokio::spawn(async move {
        agent
            .run_claimed_turn(&chat_for_turn, turn_id, MessageId::new(), 1, &tx)
            .await
    });
    let mut events = Vec::new();
    while let Some(emission) = rx.next().await {
        match emission {
            ClaimedAgentEvent::Pending { event, .. } => events.push(event),
            ClaimedAgentEvent::Committed { event, .. }
            | ClaimedAgentEvent::Recovered { event, .. } => events.push(event.event),
            ClaimedAgentEvent::Flush(acknowledge) => {
                let _ = acknowledge.send(());
            }
        }
    }
    (handle.await.unwrap().unwrap(), events)
}
