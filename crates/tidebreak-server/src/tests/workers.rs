use super::*;

/// A fixed test provider behind the same registry-enforcement boundary used by
/// production routing.
struct RegistryEnforcingResolver(Arc<dyn ModelProvider>);

#[async_trait]
impl ProviderResolver for RegistryEnforcingResolver {
    async fn resolve(&self) -> Arc<dyn ModelProvider> {
        self.0.clone()
    }

    fn enforces_model_registry(&self) -> bool {
        true
    }
}

/// Put a chat in `Allow` so a delegation runs without an approval card.
async fn allow_delegation(store: &dyn Store, chat: ChatId) {
    store
        .update_chat_metadata(
            chat,
            None,
            None,
            None,
            Some(Some(tidebreak_core::PermissionMode::Allow)),
            None,
        )
        .await
        .unwrap();
}

async fn approval_call_ids(store: &Arc<dyn Store>, chat: ChatId) -> Vec<CallId> {
    store
        .list_events(chat, 0)
        .await
        .unwrap()
        .iter()
        .filter_map(|event| match &event.event {
            AgentEvent::ApprovalRequired { call_id, .. } => Some(*call_id),
            _ => None,
        })
        .collect()
}

async fn test_app_with_skills(
    provider: Arc<dyn ModelProvider>,
) -> (Router, Arc<str>, Arc<dyn Store>, tempfile::TempDir) {
    let (dir, store) = temp_db_store("skill-retry.db").await;
    let store: Arc<dyn Store> = Arc::new(store);
    let secrets = Arc::new(MemSecrets::default());
    let mut state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(provider)),
        secrets.clone(),
        Arc::new(ToolRegistry::new()),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    state.code_execution = Some(Arc::new(
        crate::code_execution::ConfiguredExecProvider::new(
            store.clone(),
            secrets,
            dir.path().join("scratch"),
        )
        .with_skills(Some(root.join("skills"))),
    ));
    let token = state.token.clone();
    spawn_turn_worker(&state);
    (app(state), token, store, dir)
}

fn test_router_with_skills_from_store(
    store: Arc<dyn Store>,
    secrets: Arc<MemSecrets>,
    profile_dir: &std::path::Path,
    scratch_dir: &std::path::Path,
) -> (Router, Arc<str>) {
    let mut state = AppState::new(
        Config::desktop(profile_dir),
        store.clone(),
        Arc::new(FixedResolver(Arc::new(FakeProvider))),
        secrets.clone(),
        Arc::new(ToolRegistry::new()),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    state.code_execution = Some(Arc::new(
        crate::code_execution::ConfiguredExecProvider::new(store, secrets, scratch_dir)
            .with_skills(Some(root.join("skills"))),
    ));
    let token = state.token.clone();
    (app(state), token)
}

async fn post_message_body(
    router: &Router,
    bearer: &str,
    chat: ChatId,
    body: serde_json::Value,
) -> axum::response::Response {
    router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/chats/{chat}/messages"))
                .header(header::AUTHORIZATION, bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn configured_model_is_used_for_the_turn() {
    let recorder = RecordingProvider::default();
    let (router, token, store, _dir) = test_app_with(Arc::new(recorder.clone())).await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;

    // Configure the model, then run a turn.
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/settings")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"model": "claude-configured"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    assert_eq!(
        send_message(&router, &bearer, chat.id, "hi").await,
        StatusCode::ACCEPTED
    );
    wait_for_turn(&store, chat.id).await;

    assert!(
        recorder
            .models
            .lock()
            .unwrap()
            .iter()
            .any(|m| m == "claude-configured"),
        "the turn should run against the configured model"
    );
}

#[tokio::test]
async fn workspace_dir_is_not_an_accepted_product_field() {
    let (router, token, _store, _dir) = test_app().await;
    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/chats")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"workspace_dir": "/tmp/legacy"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let info: AgentErrorInfo = json_body(response).await;
    assert_eq!(info.kind, "bad_request");
}

#[tokio::test]
async fn unknown_chat_is_404() {
    let (router, token, _store, _dir) = test_app().await;
    let response = router
        .oneshot(
            Request::builder()
                .uri(format!("/chats/{}", ChatId::new()))
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread")]
async fn post_message_runs_a_turn_and_journals_its_events() {
    let (router, token, store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;

    assert_eq!(
        send_message(&router, &bearer, chat.id, "hello").await,
        StatusCode::ACCEPTED
    );

    let events = wait_for_turn(&store, chat.id).await;
    assert!(matches!(events[0].event, AgentEvent::TurnStarted { .. }));
    assert!(events
        .iter()
        .any(|e| matches!(&e.event, AgentEvent::TextDelta { text } if text == "hi")));
    assert!(events
        .iter()
        .any(|e| matches!(e.event, AgentEvent::TurnCompleted { .. })));
}

#[tokio::test(flavor = "multi_thread")]
async fn worker_commits_a_mid_stream_refusal_with_its_partial_output() {
    let (router, token, store, _dir) = test_app_with(Arc::new(MidStreamRefusalProvider)).await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;

    assert_eq!(
        send_message(&router, &bearer, chat.id, "unsafe request").await,
        StatusCode::ACCEPTED
    );

    let events = wait_for_turn(&store, chat.id).await;
    assert!(events.iter().any(
        |event| matches!(&event.event, AgentEvent::TextDelta { text } if text == "Visible partial answer")
    ));
    assert!(events.iter().any(|event| {
        matches!(
            &event.event,
            AgentEvent::TurnRefused { refusal, .. }
                if refusal.category() == Some("general_harms") && refusal.partial_output()
        )
    }));

    let turn = store.list_turns(chat.id).await.unwrap().pop().unwrap();
    assert_eq!(turn.status, TurnRunStatus::Completed);

    let response = router
        .oneshot(
            Request::builder()
                .uri(format!("/chats/{}/messages", chat.id))
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let transcript: serde_json::Value = json_body(response).await;
    let output = transcript["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["content"] == "Visible partial answer")
        .expect("the partial output remains a durable assistant message");
    let terminal_turn = transcript["terminal_turns"]
        .as_array()
        .unwrap()
        .iter()
        .find(|terminal_turn| terminal_turn["message_id"] == output["id"])
        .expect("the durable assistant message retains its terminal outcome");
    assert_eq!(terminal_turn["status"], "completed");
    assert_eq!(terminal_turn["refusal"]["category"], "general_harms");
    assert_eq!(terminal_turn["refusal"]["partial_output"], true);
}

#[tokio::test(flavor = "multi_thread")]
async fn foreground_spawn_is_nonblocking_and_ordered_wait_resumes_with_child_result() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let provider = Arc::new(SandboxRoundTripProvider::default());
    let mut tools = ToolRegistry::new();
    tools.register_foreground_agent_orchestration();
    let state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(provider.clone())),
        Arc::new(MemSecrets::default()),
        Arc::new(tools),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    let token = state.token.clone();
    spawn_turn_worker(&state);
    let sandbox_worker = sandbox_agent_run_worker::SandboxAgentRunWorker::new(
        state.store.clone(),
        state.secrets.clone(),
        state.resolver.clone(),
        state.agent_run_wake.clone(),
        state.turn_job_wake.clone(),
        state.events.clone(),
        state.agent_config.clone(),
        None,
        sandbox_agent_run_worker::SandboxAgentRunWorkerConfig::default(),
    );
    tokio::spawn(sandbox_worker.run());
    let router = app(state);
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    // Delegation is a Sensitive action, so anything short of `Allow` would park
    // this turn on an approval card nobody is here to answer.
    allow_delegation(store.as_ref(), chat.id).await;

    assert_eq!(
        send_message(&router, &bearer, chat.id, "delegate this").await,
        StatusCode::ACCEPTED
    );
    let events = wait_for_turn(&store, chat.id).await;
    assert!(matches!(
        events.last().map(|event| &event.event),
        Some(AgentEvent::TurnCompleted { .. })
    ));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.event, AgentEvent::ToolCallCompleted { .. }))
            .count(),
        3
    );
    assert!(events
        .iter()
        .any(|event| matches!(event.event, AgentEvent::StreamInterrupted)));
    assert!(!events
        .iter()
        .any(|event| matches!(event.event, AgentEvent::TurnFailed { .. })));

    let runs = store.list_agent_runs(chat.id).await.unwrap();
    let children = runs
        .iter()
        .filter(|run| run.parent_id.is_some())
        .collect::<Vec<_>>();
    assert_eq!(children.len(), 2);
    assert!(children
        .iter()
        .all(|child| child.status == AgentRunStatus::Completed));
    let parent = tidebreak_core::AgentRunId::foreground_for_chat(chat.id);
    let inbox = store.list_agent_run_inbox(parent).await.unwrap();
    assert_eq!(inbox.len(), 2);
    assert!(inbox.iter().all(|entry| {
        entry.status == AgentRunInboxStatus::Consumed
            && entry.claim_count == 1
            && entry.consumed_lease_token.is_some()
    }));
    assert!(inbox
        .iter()
        .any(|entry| entry.result.text == "first child result"));
    assert!(inbox
        .iter()
        .any(|entry| entry.result.text == "second child result"));
    let first_delivery = inbox
        .iter()
        .find(|entry| entry.result.text == "first child result")
        .unwrap()
        .delivered_at;
    let second_delivery = inbox
        .iter()
        .find(|entry| entry.result.text == "second child result")
        .unwrap()
        .delivered_at;
    assert!(second_delivery < first_delivery);

    let turn = store.list_turns(chat.id).await.unwrap().pop().unwrap();
    assert_eq!(turn.status, TurnRunStatus::Completed);
    assert_eq!(
        turn.claim_count, 4,
        "two spawn continuations and ordered wait each require a fresh lease"
    );
    let requests = provider.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 7);
    let foreground = requests
        .iter()
        .filter(|request| {
            request
                .tools
                .iter()
                .any(|tool| tool.name == tidebreak_core::SPAWN_SANDBOX_AGENT_TOOL)
        })
        .collect::<Vec<_>>();
    let sandbox = requests
        .iter()
        .filter(|request| {
            request
                .tools
                .iter()
                .any(|tool| tool.name == tidebreak_core::SANDBOX_WEB_SEARCH_TOOL)
        })
        .collect::<Vec<_>>();
    assert_eq!(foreground.len(), 5);
    assert_eq!(sandbox.len(), 2);
    assert!(foreground.iter().all(|request| request
        .tools
        .iter()
        .any(|tool| tool.name == tidebreak_core::WAIT_FOR_AGENTS_TOOL)));
    assert!(sandbox
        .iter()
        .all(|request| request.tools.iter().all(|tool| {
            !matches!(
                tool.name.as_str(),
                tidebreak_core::SPAWN_SANDBOX_AGENT_TOOL | tidebreak_core::WAIT_FOR_AGENTS_TOOL
            )
        })));
    assert!(
        foreground[3].messages.iter().any(|message| {
            message.role == Role::System && message.content.iter().any(|block| {
                matches!(
                    block,
                    ContentBlock::Text { text }
                        if text.contains("cannot finish yet") && text.contains("wait_for_agents")
                )
            })
        }),
        "premature completion must produce a fixed wait correction"
    );
    assert!(foreground[4].messages.iter().any(|message| {
        message.content.iter().any(|block| {
            matches!(
                block,
            ContentBlock::ToolResult { content, .. }
                if content.contains("first child result") && content.contains("second child result")
            )
        })
    }));
    let calls = store.list_tool_calls(chat.id).await.unwrap();
    assert_eq!(calls.len(), 3);
    assert!(calls.iter().all(|call| {
        call.execution == tidebreak_core::ToolCallExecution::Orchestration
            && call.status == tidebreak_core::ToolCallStatus::Completed
    }));
    assert!(calls
        .iter()
        .any(|call| call.name == tidebreak_core::SPAWN_SANDBOX_AGENT_TOOL));
    assert!(calls
        .iter()
        .any(|call| call.name == tidebreak_core::WAIT_FOR_AGENTS_TOOL));
    let wait_result = calls
        .iter()
        .find(|call| call.name == tidebreak_core::WAIT_FOR_AGENTS_TOOL)
        .and_then(|call| call.result.as_deref())
        .expect("ordered wait should persist its bounded result");
    assert!(
        wait_result.find("first child result").unwrap()
            < wait_result.find("second child result").unwrap(),
        "wait result must preserve spawn/request order despite reverse completion"
    );
    let messages = store.list_messages(chat.id).await.unwrap();
    assert!(messages
        .iter()
        .any(|message| message.content == "parent completed after ordered wait"));
    assert!(!messages
        .iter()
        .any(|message| message.content.contains("premature parent answer")));
}

/// Multiple delegations in one model step are rejected before approval or
/// child admission, then the model gets one clean retry.
#[tokio::test(flavor = "multi_thread")]
async fn a_spawn_batch_is_rejected_before_any_approval_or_child_admission() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let provider = Arc::new(GatedSpawnBatchProvider::default());
    let mut tools = ToolRegistry::new();
    tools.register_foreground_agent_orchestration();
    let state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(provider.clone())),
        Arc::new(MemSecrets::default()),
        Arc::new(tools),
        AgentConfig {
            model: "fake".into(),
            max_steps: 4,
            ..AgentConfig::default()
        },
    );
    let token = state.token.clone();
    spawn_turn_worker(&state);
    let router = app(state);
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;

    assert_eq!(
        send_message(&router, &bearer, chat.id, "delegate both questions").await,
        StatusCode::ACCEPTED
    );

    let events = wait_for_turn(&store, chat.id).await;
    assert!(matches!(
        events.last().map(|event| &event.event),
        Some(AgentEvent::TurnCompleted { .. })
    ));
    assert_eq!(
        provider.foreground_calls.load(Ordering::SeqCst),
        2,
        "the invalid batch should receive exactly one corrective retry"
    );
    assert!(approval_call_ids(&store, chat.id).await.is_empty());
    assert!(store.list_agent_runs(chat.id).await.unwrap().is_empty());
    assert!(store
        .list_tool_calls(chat.id)
        .await
        .unwrap()
        .iter()
        .all(|call| call.name != tidebreak_core::SPAWN_SANDBOX_AGENT_TOOL));
}

#[tokio::test(flavor = "multi_thread")]
async fn worker_drains_a_turn_queued_before_startup() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
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
        memory_incognito: false,
        created_at: chrono::Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    assert!(matches!(
        store
            .accept_turn(turn_id, chat.id, "fake", "queued before startup")
            .await
            .unwrap(),
        tidebreak_core::AcceptTurnOutcome::Accepted(_)
    ));

    let state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(Arc::new(FakeProvider))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    spawn_turn_worker(&state);

    let events = wait_for_turn(&store, chat.id).await;
    assert!(matches!(events[0].event, AgentEvent::TurnStarted { turn_id: id } if id == turn_id));
    assert!(matches!(
        events.last().map(|event| &event.event),
        Some(AgentEvent::TurnCompleted { .. })
    ));
    assert_eq!(store.list_messages(chat.id).await.unwrap().len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn worker_rejects_an_already_accepted_plain_gateway_model_before_egress() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("legacy-gateway-turn.db").display()
        ))
        .await
        .unwrap(),
    );
    providers::write_gateway_snapshot(
        store.as_ref(),
        &providers::GatewayModelSnapshot {
            gateway_url: "https://gateway.example/".into(),
            installation_id: Some("install-1".into()),
            models: vec![providers::CustomModelConfig {
                id: "sample-claude".into(),
                upstream_id: Some("claude-opus-5".into()),
                ..Default::default()
            }],
            model_protocols: Default::default(),
            model_reasoning_efforts: Default::default(),
            member_catalog: Some("v1".into()),
            catalog_etag: None,
        },
    )
    .await
    .unwrap();

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
        memory_incognito: false,
        created_at: chrono::Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    assert!(matches!(
        store
            .accept_turn(
                turn_id,
                chat.id,
                "model_gateway::sample-claude",
                "queued by an older Tidebreak build",
            )
            .await
            .unwrap(),
        tidebreak_core::AcceptTurnOutcome::Accepted(_)
    ));

    let recorder = RecordingProvider::default();
    let state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(RegistryEnforcingResolver(Arc::new(recorder.clone()))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        AgentConfig::default(),
    );
    spawn_turn_worker(&state);

    let events = wait_for_turn(&store, chat.id).await;
    assert!(events.iter().any(|event| matches!(
        &event.event,
        AgentEvent::TurnFailed { error, .. } if error.kind == "model_provider_unavailable"
    )));
    let turn = store
        .get_turn(turn_id)
        .await
        .unwrap()
        .expect("the rejected legacy turn remains durable");
    assert_eq!(turn.status, TurnRunStatus::Failed);
    assert_eq!(
        turn.last_error_code.as_deref(),
        Some("model_provider_unavailable")
    );
    assert_eq!(
        turn.last_error_detail.as_deref(),
        Some("managed gateway execution requires a frozen model identity")
    );
    assert!(
        recorder.models.lock().unwrap().is_empty(),
        "the invalid legacy execution identity must be rejected before provider egress"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn post_message_is_idempotent_by_turn_id_and_rejects_identity_reuse() {
    let gate = Arc::new(Notify::new());
    let (router, token, store, _dir) =
        test_app_with(Arc::new(GatedProvider { gate: gate.clone() })).await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let turn_id = TurnId::new();

    assert_eq!(
        send_message_with_id(&router, &bearer, chat.id, turn_id, "hello").await,
        StatusCode::ACCEPTED
    );
    store
        .set_setting("model", &serde_json::json!("changed-after-accept"))
        .await
        .unwrap();
    assert_eq!(
        send_message_with_id(&router, &bearer, chat.id, turn_id, "hello").await,
        StatusCode::ACCEPTED,
        "an ambiguous retry must converge even after model resolution changes"
    );
    assert_eq!(store.list_messages(chat.id).await.unwrap().len(), 1);

    assert_eq!(
        send_message_with_id(&router, &bearer, chat.id, turn_id, "different").await,
        StatusCode::CONFLICT,
        "one turn id cannot name different request data"
    );
    gate.notify_one();
    wait_for_turn(&store, chat.id).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_message_retry_converges_across_a_model_setting_race() {
    let dir = tempfile::tempdir().unwrap();
    let inner: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let injected = Arc::new(PauseTerminalStore::new(
        inner,
        entered.clone(),
        release.clone(),
    ));
    injected.do_not_pause_terminal();
    injected.pause_next_acceptance();
    let store: Arc<dyn Store> = injected;
    store
        .set_setting("model", &serde_json::json!("model-before"))
        .await
        .unwrap();
    let gate = Arc::new(Notify::new());
    let (router, token, store, _dir) =
        test_app_from_parts(Arc::new(GatedProvider { gate: gate.clone() }), store, dir);
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let turn_id = TurnId::new();
    let first_router = router.clone();
    let first_bearer = bearer.clone();
    let mut first = tokio::spawn(async move {
        send_message_with_id(
            &first_router,
            &first_bearer,
            chat.id,
            turn_id,
            "same request",
        )
        .await
    });
    let first_status = tokio::time::timeout(Duration::from_secs(2), async {
        tokio::select! {
            () = entered.notified() => None,
            status = &mut first => Some(status.expect("first request task joined")),
        }
    })
    .await
    .expect("first request reached an acceptance decision");
    assert_eq!(
        first_status, None,
        "first request completed before the injected acceptance pause"
    );

    store
        .set_setting("model", &serde_json::json!("model-after"))
        .await
        .unwrap();

    let retry_router = router.clone();
    let retry_bearer = bearer.clone();
    let mut retry = tokio::spawn(async move {
        send_message_with_id(
            &retry_router,
            &retry_bearer,
            chat.id,
            turn_id,
            "same request",
        )
        .await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut retry)
            .await
            .is_err(),
        "the exact retry must wait for the first acceptance decision"
    );

    release.notify_one();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), first)
            .await
            .expect("first acceptance completed after release")
            .unwrap(),
        StatusCode::ACCEPTED
    );
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), retry)
            .await
            .expect("retry completed after the first acceptance")
            .unwrap(),
        StatusCode::ACCEPTED
    );
    assert_eq!(store.list_turns(chat.id).await.unwrap().len(), 1);
    assert_eq!(store.list_messages(chat.id).await.unwrap().len(), 1);

    gate.notify_one();
    wait_for_turn(&store, chat.id).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn cross_process_exact_retry_waits_for_durable_admission_without_revalidation() {
    let dir = tempfile::tempdir().unwrap();
    let inner: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("cross-process.db").display()
        ))
        .await
        .unwrap(),
    );
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let injected = Arc::new(PauseTerminalStore::new(
        inner,
        entered.clone(),
        release.clone(),
    ));
    injected.do_not_pause_terminal();
    injected.pause_next_acceptance();
    let store: Arc<dyn Store> = injected;
    store
        .set_setting("model", &serde_json::json!("fake"))
        .await
        .unwrap();
    let secrets = Arc::new(MemSecrets::default());
    let (router_a, token_a) = test_router_with_skills_from_store(
        store.clone(),
        secrets.clone(),
        dir.path(),
        &dir.path().join("scratch-a"),
    );
    let (router_b, token_b) = test_router_with_skills_from_store(
        store.clone(),
        secrets,
        dir.path(),
        &dir.path().join("scratch-b"),
    );
    let bearer_a = format!("Bearer {token_a}");
    let bearer_b = format!("Bearer {token_b}");
    let chat = make_chat(&router_a, &bearer_a).await;
    let turn_id = TurnId::new();
    let request = serde_json::json!({
        "turn_id": turn_id,
        "content": "build the deck",
        "invoked_skills": ["presentations"],
    });

    let first_router = router_a.clone();
    let first_bearer = bearer_a.clone();
    let first_request = request.clone();
    let first = tokio::spawn(async move {
        post_message_body(&first_router, &first_bearer, chat.id, first_request)
            .await
            .status()
    });
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .expect("first server reserved admission before its commit");

    store
        .set_setting(
            crate::plugin_state::PLUGIN_ENABLE_STATE_SETTING,
            &serde_json::json!({"skills": {"presentations": false}}),
        )
        .await
        .unwrap();
    release.notify_one();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), first)
            .await
            .expect("first process completed")
            .unwrap(),
        StatusCode::ACCEPTED
    );
    // D4a retired the admission ledger. A concurrent in-flight retry is not
    // fenced; once the first commit lands, the fingerprint on the turn row
    // is enough and the route must not consult the skill catalog again.
    assert_eq!(
        post_message_body(&router_b, &bearer_b, chat.id, request)
            .await
            .status(),
        StatusCode::ACCEPTED
    );
    assert_eq!(store.list_turns(chat.id).await.unwrap().len(), 1);
    assert_eq!(store.list_messages(chat.id).await.unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn exact_accepted_retry_survives_invoked_skill_becoming_unavailable() {
    let gate = Arc::new(Notify::new());
    let (router, token, store, _dir) =
        test_app_with_skills(Arc::new(GatedProvider { gate: gate.clone() })).await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let turn_id = TurnId::new();
    let request = serde_json::json!({
        "turn_id": turn_id,
        "content": "build the deck",
        "invoked_skills": ["presentations"],
    });

    assert_eq!(
        post_message_body(&router, &bearer, chat.id, request.clone())
            .await
            .status(),
        StatusCode::ACCEPTED
    );
    store
        .set_setting(
            crate::plugin_state::PLUGIN_ENABLE_STATE_SETTING,
            &serde_json::json!({"skills": {"presentations": false}}),
        )
        .await
        .unwrap();

    assert_eq!(
        post_message_body(&router, &bearer, chat.id, request)
            .await
            .status(),
        StatusCode::ACCEPTED,
        "an exact accepted retry must not revalidate mutable skill state"
    );
    assert_eq!(
        post_message_body(
            &router,
            &bearer,
            chat.id,
            serde_json::json!({
                "turn_id": turn_id,
                "content": "build the deck",
                "invoked_skills": ["charts"],
            }),
        )
        .await
        .status(),
        StatusCode::CONFLICT,
        "the same accepted id cannot name a different skill identity"
    );
    assert_eq!(store.list_turns(chat.id).await.unwrap().len(), 1);
    assert_eq!(store.list_messages(chat.id).await.unwrap().len(), 1);

    gate.notify_one();
    wait_for_turn(&store, chat.id).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn exact_queued_retry_survives_invoked_skill_becoming_unavailable() {
    let gate = Arc::new(Notify::new());
    let (router, token, store, _dir) =
        test_app_with_skills(Arc::new(GatedProvider { gate: gate.clone() })).await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    assert_eq!(
        send_message_with_id(&router, &bearer, chat.id, TurnId::new(), "blocking turn").await,
        StatusCode::ACCEPTED
    );

    let queued_id = TurnId::new();
    let queued_request = serde_json::json!({
        "turn_id": queued_id,
        "content": "build the queued deck",
        "invoked_skills": ["presentations"],
        "queue": true,
    });
    assert_eq!(
        post_message_body(&router, &bearer, chat.id, queued_request.clone())
            .await
            .status(),
        StatusCode::ACCEPTED
    );
    store
        .set_setting(
            crate::plugin_state::PLUGIN_ENABLE_STATE_SETTING,
            &serde_json::json!({"skills": {"presentations": false}}),
        )
        .await
        .unwrap();

    assert_eq!(
        post_message_body(&router, &bearer, chat.id, queued_request)
            .await
            .status(),
        StatusCode::ACCEPTED,
        "an exact queued retry must not revalidate mutable skill state"
    );
    assert_eq!(
        post_message_body(
            &router,
            &bearer,
            chat.id,
            serde_json::json!({
                "turn_id": queued_id,
                "content": "different queued input",
                "invoked_skills": ["presentations"],
                "queue": true,
            }),
        )
        .await
        .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(store.list_queued_turns(chat.id).await.unwrap().len(), 1);

    gate.notify_one();
    wait_for_turn(&store, chat.id).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn retrying_a_later_queued_turn_never_bypasses_fifo() {
    let gate = Arc::new(Notify::new());
    let (router, token, store, _dir) =
        test_app_with(Arc::new(GatedProvider { gate: gate.clone() })).await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let blocking_id = TurnId::new();
    assert_eq!(
        send_message_with_id(&router, &bearer, chat.id, blocking_id, "blocking turn",).await,
        StatusCode::ACCEPTED
    );

    let first_queued = TurnId::new();
    let second_queued = TurnId::new();
    for (turn_id, content) in [
        (first_queued, "first queued"),
        (second_queued, "second queued"),
    ] {
        assert_eq!(
            post_message_body(
                &router,
                &bearer,
                chat.id,
                serde_json::json!({
                    "turn_id": turn_id,
                    "content": content,
                    "queue": true,
                }),
            )
            .await
            .status(),
            StatusCode::ACCEPTED
        );
    }

    gate.notify_one();
    wait_for_turn(&store, chat.id).await;
    assert_eq!(
        post_message_body(
            &router,
            &bearer,
            chat.id,
            serde_json::json!({
                "turn_id": second_queued,
                "content": "second queued",
                "queue": false,
            }),
        )
        .await
        .status(),
        StatusCode::ACCEPTED
    );

    let turns = store.list_turns(chat.id).await.unwrap();
    assert_eq!(turns.len(), 1, "retry traffic must not promote queued work");
    assert_eq!(turns[0].id, blocking_id);
    let queued = store.list_queued_turns(chat.id).await.unwrap();
    assert_eq!(
        queued.iter().map(|turn| turn.id).collect::<Vec<_>>(),
        [first_queued, second_queued],
        "the FIFO queue must remain unchanged"
    );
}

#[tokio::test]
async fn queued_retry_compares_attachment_identity_before_blob_resolution() {
    let (router, token, store, _dir) = test_app_without_turn_worker().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let queued_id = TurnId::new();
    let attachment_id = uuid::Uuid::new_v4();
    let now = chrono::Utc::now();
    store
        .enqueue_queued_turn(&tidebreak_core::QueuedTurn {
            id: queued_id,
            chat_id: chat.id,
            content: "queued image".into(),
            attachments: vec![attachment_id],
            file_attachments: Vec::new(),
            invoked_skills: Vec::new(),
            voice_input_used: false,
            position: 0,
            created_at: now,
            updated_at: now,
        })
        .await
        .unwrap();

    assert_eq!(
        post_message_body(
            &router,
            &bearer,
            chat.id,
            serde_json::json!({
                "turn_id": queued_id,
                "content": "queued image",
                "attachments": [attachment_id],
                "queue": true,
            }),
        )
        .await
        .status(),
        StatusCode::ACCEPTED,
        "the exact durable retry must not require the blob to resolve again"
    );
    assert_eq!(
        post_message_body(
            &router,
            &bearer,
            chat.id,
            serde_json::json!({
                "turn_id": queued_id,
                "content": "queued image",
                "attachments": [uuid::Uuid::new_v4()],
                "queue": true,
            }),
        )
        .await
        .status(),
        StatusCode::CONFLICT,
        "a queued id cannot silently change image identity"
    );
}

#[tokio::test]
async fn queued_turn_id_cannot_be_accepted_by_another_chat() {
    let (router, token, store, _dir) = test_app_without_turn_worker().await;
    let bearer = format!("Bearer {token}");
    let first_chat = make_chat(&router, &bearer).await;
    let second_chat = make_chat(&router, &bearer).await;
    let queued_id = TurnId::new();
    let queued = tidebreak_core::QueuedTurn {
        id: queued_id,
        chat_id: first_chat.id,
        content: "owned by the first chat".into(),
        attachments: Vec::new(),
        file_attachments: Vec::new(),
        invoked_skills: Vec::new(),
        voice_input_used: false,
        position: 0,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };
    store.enqueue_queued_turn(&queued).await.unwrap();

    assert_eq!(
        post_message_body(
            &router,
            &bearer,
            second_chat.id,
            serde_json::json!({
                "turn_id": queued_id,
                "content": "owned by the first chat",
            }),
        )
        .await
        .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(
        post_message_body(
            &router,
            &bearer,
            first_chat.id,
            serde_json::json!({
                "turn_id": queued_id,
                "content": "owned by the first chat",
                "queue": true,
            }),
        )
        .await
        .status(),
        StatusCode::ACCEPTED
    );
    assert!(store.list_turns(second_chat.id).await.unwrap().is_empty());
    let remaining = store.list_queued_turns(first_chat.id).await.unwrap();
    assert_eq!(remaining.len(), 1);
    assert!(remaining[0].same_request(&queued));
}

#[tokio::test(flavor = "multi_thread")]
async fn sandbox_container_routing_preserves_in_process_task_and_deadline_shape() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("container.db").display()
        ))
        .await
        .unwrap(),
    );
    let provider = Arc::new(SandboxRoundTripProvider::default());
    let mut tools = ToolRegistry::new();
    tools.register_foreground_agent_orchestration();
    let state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(provider)),
        Arc::new(MemSecrets::default()),
        Arc::new(tools),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    let token = state.token.clone();
    spawn_turn_worker_with_config(
        &state,
        engine::internal::leg::LegDriverConfig {
            sandbox_spawn_execution_location: tidebreak_core::AgentRunExecutionLocation::Container,
            ..engine::internal::leg::LegDriverConfig::default()
        },
    );
    let router = app(state);
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    allow_delegation(store.as_ref(), chat.id).await;
    assert_eq!(
        send_message(&router, &bearer, chat.id, "delegate this").await,
        StatusCode::ACCEPTED
    );
    let container_child = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(child) = store
                .list_agent_runs(chat.id)
                .await
                .unwrap()
                .into_iter()
                .find(|run| run.parent_id.is_some())
            {
                break child;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("foreground spawn should admit a container child");
    assert_eq!(
        container_child.execution_location,
        tidebreak_core::AgentRunExecutionLocation::Container
    );
    assert_eq!(
        container_child.input.as_deref(),
        Some("Return the first child result.")
    );

    let (_reference_dir, reference_store) = temp_db_store("in-process-reference.db").await;
    let reference_store: Arc<dyn Store> = Arc::new(reference_store);
    let reference_chat = Chat {
        id: ChatId::new(),
        project_id: None,
        title: Some("in-process reference".into()),
        model: Some("fake".into()),
        reasoning_effort: None,
        permission_mode: None,
        network_policy: Default::default(),
        attachment_revision: 0,
        root_attachments: Vec::new(),
        memory_incognito: false,
        created_at: chrono::Utc::now(),
    };
    reference_store.create_chat(&reference_chat).await.unwrap();
    let reference_turn_id = TurnId::new();
    reference_store
        .accept_turn(
            reference_turn_id,
            reference_chat.id,
            "fake",
            "reference admission",
        )
        .await
        .unwrap();
    let reference_lease = uuid::Uuid::new_v4();
    let now = chrono::Utc::now();
    let reference_turn = reference_store
        .claim_turn(reference_lease, now, now + chrono::Duration::minutes(5))
        .await
        .unwrap()
        .turn
        .unwrap();
    let in_process_child = match reference_store
        .admit_sandbox_agent_run(
            reference_turn.id,
            CallId::new(),
            "Return the first child result.",
            reference_lease,
            reference_turn.steer_revision,
            tidebreak_core::AgentRun::MAX_CONCURRENCY_LIMIT,
            chrono::Utc::now(),
        )
        .await
        .unwrap()
        .unwrap()
    {
        tidebreak_core::AdmitSandboxAgentRunOutcome::Accepted { child, .. } => child,
        outcome => panic!("unexpected in-process reference admission: {outcome:?}"),
    };

    assert_eq!(container_child.input, in_process_child.input);
    assert_eq!(
        container_child.deadline_at.unwrap() - container_child.created_at,
        in_process_child.deadline_at.unwrap() - in_process_child.created_at
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn worker_recovers_ambiguous_claim_and_completion_with_exact_receipts() {
    let dir = tempfile::tempdir().unwrap();
    let inner: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let injected = Arc::new(PauseTerminalStore::new(
        inner,
        Arc::new(Notify::new()),
        Arc::new(Notify::new()),
    ));
    injected.do_not_pause_terminal();
    injected.fail_after_next_claim_commit();
    injected.fail_after_next_completion_commit();
    injected.fail_next_terminal_recovery();
    let store: Arc<dyn Store> = injected.clone();
    let (router, token, store, _dir) = test_app_from_parts_with_worker_config(
        Arc::new(FakeProvider),
        store,
        dir,
        engine::internal::leg::LegDriverConfig {
            // Keep this failure-injection test inside the committed claim's
            // lease even when a loaded test host delays the retry task.
            lease: Duration::from_secs(5 * 60),
            failure_delay: Duration::from_millis(10),
            failure_delay_cap: Duration::from_millis(40),
            retry: fast_retry_schedule(),
            max_concurrency: 1,
            ..engine::internal::leg::LegDriverConfig::default()
        },
    );
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;

    assert_eq!(
        send_message(&router, &bearer, chat.id, "recover me").await,
        StatusCode::ACCEPTED
    );
    let events = wait_for_turn(&store, chat.id).await;
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.event, AgentEvent::TurnStarted { .. }))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.event, AgentEvent::TurnCompleted { .. }))
            .count(),
        1
    );
    let messages = store.list_messages(chat.id).await.unwrap();
    assert_eq!(messages.len(), 2, "input and exact terminal output only");
    tokio::time::timeout(Duration::from_secs(5), async {
        while injected.terminal_recovery_calls() < 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("transient exact-terminal recovery is retried");

    let next_chat = make_chat(&router, &bearer).await;
    assert_eq!(
        send_message(&router, &bearer, next_chat.id, "lane is free").await,
        StatusCode::ACCEPTED
    );
    let next_events = wait_for_turn(&store, next_chat.id).await;
    assert!(matches!(
        next_events.last().map(|event| &event.event),
        Some(AgentEvent::TurnCompleted { .. })
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn worker_retries_a_transient_provider_failure_without_a_terminal_event() {
    struct FailOnceProvider {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ModelProvider for FailOnceProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("retry-once")
        }

        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                return Err(AgentError::Provider("injected transient failure".into()));
            }
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

    let calls = Arc::new(AtomicUsize::new(0));
    let (router, token, store, _dir) = test_app_with(Arc::new(FailOnceProvider {
        calls: calls.clone(),
    }))
    .await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    assert_eq!(
        send_message(&router, &bearer, chat.id, "retry me").await,
        StatusCode::ACCEPTED
    );

    let events = wait_for_turn(&store, chat.id).await;
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(!events
        .iter()
        .any(|event| matches!(event.event, AgentEvent::TurnFailed { .. })));
    assert!(matches!(
        events.last().map(|event| &event.event),
        Some(AgentEvent::TurnCompleted { .. })
    ));
    let turn = store.list_turns(chat.id).await.unwrap().pop().unwrap();
    assert_eq!(turn.attempt_count, 2);
    assert_eq!(turn.status, TurnRunStatus::Completed);
}

/// A retryable provider failure during the wrap-up call resumes into the
/// wrap-up, not a `max_steps_exceeded` failure (#1181). The retrying lease
/// segment arrives with zero remaining step budget — the budget was spent
/// before the wrap-up, which is deliberately outside it — and must still be
/// allowed to make the one tool-free closing call.
#[tokio::test(flavor = "multi_thread")]
async fn zero_budget_resume_runs_the_wrap_up_instead_of_failing() {
    struct WrapUpFailOnce {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ModelProvider for WrapUpFailOnce {
        fn id(&self) -> ProviderId {
            ProviderId::new("wrap-up-fail-once")
        }

        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            match self.calls.fetch_add(1, Ordering::SeqCst) {
                // The only budgeted step: spend it on a tool call.
                0 => Ok(stream::iter(vec![
                    ProviderEvent::ToolCallStarted {
                        index: 0,
                        id: "call_1".into(),
                        name: "read_file".into(),
                    },
                    ProviderEvent::ToolCallArgsDelta {
                        index: 0,
                        fragment: "{}".into(),
                    },
                    ProviderEvent::Stop {
                        reason: StopReason::ToolUse,
                    },
                ])
                .boxed()),
                // The wrap-up call fails retryably; the retry claims the turn
                // with its whole step budget already consumed.
                1 => Err(AgentError::Provider("injected wrap-up failure".into())),
                _ => Ok(stream::iter(vec![
                    ProviderEvent::TextDelta {
                        text: "wrapped up after resume".into(),
                    },
                    ProviderEvent::Stop {
                        reason: StopReason::EndTurn,
                    },
                ])
                .boxed()),
            }
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(Arc::new(WrapUpFailOnce {
            calls: calls.clone(),
        }))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        AgentConfig {
            model: "fake".into(),
            max_steps: 1,
            ..AgentConfig::default()
        },
    );
    let token = state.token.clone();
    spawn_turn_worker(&state);
    let router = app(state);
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    assert_eq!(
        send_message(&router, &bearer, chat.id, "go").await,
        StatusCode::ACCEPTED
    );

    let events = wait_for_turn(&store, chat.id).await;
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert!(matches!(
        events.last().map(|event| &event.event),
        Some(AgentEvent::TurnCompleted { .. })
    ));
    let turn = store.list_turns(chat.id).await.unwrap().pop().unwrap();
    assert_eq!(turn.status, TurnRunStatus::Completed);
    assert_eq!(turn.attempt_count, 2);
    // The wrap-up is outside the budget: only the tool step is counted.
    assert_eq!(turn.model_steps, 1);
    assert!(store
        .list_messages(chat.id)
        .await
        .unwrap()
        .iter()
        .any(|message| message.content == "wrapped up after resume"));
}

#[tokio::test(flavor = "multi_thread")]
async fn scanner_won_failure_does_not_wedge_the_only_worker_lane() {
    struct FailOnceProvider {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl ModelProvider for FailOnceProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("fail-once")
        }

        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                return Err(AgentError::Provider("injected first failure".into()));
            }
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

    let (router, token, store, _dir) = test_app_with_scanner_resolution_race(
        Arc::new(FailOnceProvider {
            calls: AtomicUsize::new(0),
        }),
        PauseTerminalStore::let_scan_win_next_failure_resolution,
    )
    .await;
    let bearer = format!("Bearer {token}");
    let failed_chat = make_chat(&router, &bearer).await;
    assert_eq!(
        send_message(&router, &bearer, failed_chat.id, "fail first").await,
        StatusCode::ACCEPTED
    );
    let failed_events = wait_for_turn(&store, failed_chat.id).await;
    assert!(matches!(
        failed_events.last().map(|event| &event.event),
        Some(AgentEvent::TurnFailed { error }) if error.kind == "lease_expired"
    ));

    let next_chat = make_chat(&router, &bearer).await;
    assert_eq!(
        send_message(&router, &bearer, next_chat.id, "use freed lane").await,
        StatusCode::ACCEPTED
    );
    let next_events = wait_for_turn(&store, next_chat.id).await;
    assert!(matches!(
        next_events.last().map(|event| &event.event),
        Some(AgentEvent::TurnCompleted { .. })
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn scanner_won_cancellation_does_not_wedge_the_only_worker_lane() {
    struct UsageGatedProvider {
        entered: Arc<Notify>,
        gate: Arc<Notify>,
    }

    #[async_trait]
    impl ModelProvider for UsageGatedProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("usage-gated")
        }

        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            let entered = self.entered.clone();
            let gate = self.gate.clone();
            Ok(stream::iter(vec![ProviderEvent::Usage(Usage {
                input_tokens: 7,
                output_tokens: 3,
                ..Usage::default()
            })])
            .chain(
                stream::once(async move {
                    entered.notify_one();
                    gate.notified().await;
                    stream::iter(vec![
                        ProviderEvent::TextDelta {
                            text: "gated answer".into(),
                        },
                        ProviderEvent::Stop {
                            reason: StopReason::EndTurn,
                        },
                    ])
                })
                .flatten(),
            )
            .boxed())
        }
    }

    let entered = Arc::new(Notify::new());
    let gate = Arc::new(Notify::new());
    let (router, token, store, _dir) = test_app_with_scanner_resolution_race(
        Arc::new(UsageGatedProvider {
            entered: entered.clone(),
            gate: gate.clone(),
        }),
        PauseTerminalStore::let_scan_win_next_cancellation_ack,
    )
    .await;
    let bearer = format!("Bearer {token}");
    let cancelled_chat = make_chat(&router, &bearer).await;
    let turn_id = TurnId::new();
    assert_eq!(
        send_message_with_id(&router, &bearer, cancelled_chat.id, turn_id, "cancel first",).await,
        StatusCode::ACCEPTED
    );
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .expect("provider consumed nonzero usage before parking");
    assert_eq!(
        cancel_turn(&router, &bearer, cancelled_chat.id, turn_id).await,
        StatusCode::ACCEPTED
    );
    let cancelled_events = wait_for_turn(&store, cancelled_chat.id).await;
    assert!(matches!(
        cancelled_events.last().map(|event| &event.event),
        Some(AgentEvent::TurnCancelled { usage }) if *usage == Usage::default()
    ));

    gate.notify_one();
    let next_chat = make_chat(&router, &bearer).await;
    assert_eq!(
        send_message(&router, &bearer, next_chat.id, "use freed lane").await,
        StatusCode::ACCEPTED
    );
    let next_events = wait_for_turn(&store, next_chat.id).await;
    assert!(matches!(
        next_events.last().map(|event| &event.event),
        Some(AgentEvent::TurnCompleted { .. })
    ));
}

#[tokio::test(start_paused = true)]
async fn worker_renews_a_near_expiry_ambiguous_claim_before_execution() {
    let dir = tempfile::tempdir().unwrap();
    let inner: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let injected = Arc::new(PauseTerminalStore::new(
        inner,
        entered.clone(),
        release.clone(),
    ));
    injected.do_not_pause_terminal();
    injected.pause_after_next_claim_commit();
    injected.fail_after_next_heartbeat_commit();
    let store: Arc<dyn Store> = injected;
    let gate = Arc::new(Notify::new());
    let state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(Arc::new(GatedProvider {
            gate: gate.clone(),
        }))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    let worker = engine::internal::leg::LegDriver::new(
        state.store.clone(),
        state.resolver.clone(),
        state.secrets.clone(),
        state.provisioned_policy.clone(),
        state.os_policy.clone(),
        state.tools.clone(),
        state.approvals.clone(),
        state.events.clone(),
        state.active_turns.clone(),
        state.turn_job_wake.clone(),
        state.agent_run_wake.clone(),
        state.queued_turn_wake.clone(),
        state.agent_config.clone(),
        None,
        engine::internal::leg::LegDriverConfig {
            lease: Duration::from_millis(600),
            heartbeat: Duration::from_millis(200),
            steer_poll: Duration::from_millis(10),
            idle_min: Duration::from_millis(10),
            idle_cap: Duration::from_millis(20),
            failure_delay: Duration::from_millis(10),
            failure_delay_cap: Duration::from_millis(40),
            retry: fast_retry_schedule(),
            max_concurrency: 1,
            sandbox_spawn_execution_location: tidebreak_core::AgentRunExecutionLocation::InProcess,
        },
    );
    let token = state.token.clone();
    tokio::spawn(worker.run());
    let router = app(state);
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;

    assert_eq!(
        send_message(&router, &bearer, chat.id, "renew before work").await,
        StatusCode::ACCEPTED
    );
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .expect("claim committed before its delayed response");
    tokio::time::advance(Duration::from_millis(450)).await;
    release.notify_one();
    for _ in 0..100 {
        if store
            .list_events(chat.id, 0)
            .await
            .unwrap()
            .iter()
            .any(|event| matches!(event.event, AgentEvent::TurnStarted { .. }))
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tokio::time::advance(Duration::from_millis(250)).await;
    gate.notify_one();

    let events = wait_for_turn(&store, chat.id).await;
    assert!(matches!(
        events.last().map(|event| &event.event),
        Some(AgentEvent::TurnCompleted { .. })
    ));
}

#[tokio::test(start_paused = true)]
async fn worker_heartbeats_while_event_journaling_is_blocked() {
    let dir = tempfile::tempdir().unwrap();
    let inner: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let injected = Arc::new(PauseTerminalStore::new(
        inner,
        entered.clone(),
        release.clone(),
    ));
    injected.do_not_pause_terminal();
    injected.pause_next_nonterminal_event();
    let store: Arc<dyn Store> = injected;
    let state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(Arc::new(FakeProvider))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    let worker = engine::internal::leg::LegDriver::new(
        state.store.clone(),
        state.resolver.clone(),
        state.secrets.clone(),
        state.provisioned_policy.clone(),
        state.os_policy.clone(),
        state.tools.clone(),
        state.approvals.clone(),
        state.events.clone(),
        state.active_turns.clone(),
        state.turn_job_wake.clone(),
        state.agent_run_wake.clone(),
        state.queued_turn_wake.clone(),
        state.agent_config.clone(),
        None,
        engine::internal::leg::LegDriverConfig {
            lease: Duration::from_millis(250),
            heartbeat: Duration::from_millis(50),
            steer_poll: Duration::from_millis(10),
            idle_min: Duration::from_millis(10),
            idle_cap: Duration::from_millis(20),
            failure_delay: Duration::from_millis(10),
            failure_delay_cap: Duration::from_millis(40),
            retry: fast_retry_schedule(),
            max_concurrency: 1,
            sandbox_spawn_execution_location: tidebreak_core::AgentRunExecutionLocation::InProcess,
        },
    );
    let token = state.token.clone();
    tokio::spawn(worker.run());
    let router = app(state);
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;

    assert_eq!(
        send_message(&router, &bearer, chat.id, "keep alive").await,
        StatusCode::ACCEPTED
    );
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .expect("worker reached the blocked event append");
    tokio::time::advance(Duration::from_millis(400)).await;
    release.notify_one();

    let events = wait_for_turn(&store, chat.id).await;
    assert!(
        matches!(
            events.last().map(|event| &event.event),
            Some(AgentEvent::TurnCompleted { .. })
        ),
        "unexpected journal: {events:?}; turns: {:?}",
        store.list_turns(chat.id).await.unwrap()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_nul_agent_output_fails_without_wedging_the_worker() {
    struct NulProvider;

    #[async_trait]
    impl ModelProvider for NulProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("nul")
        }

        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            Ok(stream::iter(vec![
                ProviderEvent::TextDelta {
                    text: "bad\0output".into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ])
            .boxed())
        }
    }

    let (router, token, store, _dir) = test_app_with(Arc::new(NulProvider)).await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    assert_eq!(
        send_message(&router, &bearer, chat.id, "produce invalid output").await,
        StatusCode::ACCEPTED
    );

    let events = wait_for_turn(&store, chat.id).await;
    assert!(matches!(
        events.last().map(|event| &event.event),
        Some(AgentEvent::TurnFailed { error }) if error.kind == "invalid_agent_output"
    ));
    assert_eq!(store.list_messages(chat.id).await.unwrap().len(), 1);
}

/// A response with no text and no tool calls is not an answer, so it must not
/// end the turn as a success. The remedy is to ask again on the same
/// transcript, which is what the user would do by hand.
#[tokio::test(flavor = "multi_thread")]
async fn an_empty_model_response_does_not_complete_the_turn() {
    struct EmptyOnceProvider {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ModelProvider for EmptyOnceProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("empty-once")
        }

        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                return Ok(stream::iter(vec![ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                }])
                .boxed());
            }
            Ok(stream::iter(vec![
                ProviderEvent::TextDelta {
                    text: "here is the answer".into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ])
            .boxed())
        }
    }

    let calls = Arc::new(AtomicUsize::new(0));
    let (router, token, store, _dir) = test_app_with(Arc::new(EmptyOnceProvider {
        calls: calls.clone(),
    }))
    .await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    assert_eq!(
        send_message(&router, &bearer, chat.id, "say something").await,
        StatusCode::ACCEPTED
    );

    let events = wait_for_turn(&store, chat.id).await;
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(matches!(
        events.last().map(|event| &event.event),
        Some(AgentEvent::TurnCompleted { .. })
    ));
    let turn = store.list_turns(chat.id).await.unwrap().pop().unwrap();
    assert_eq!(turn.attempt_count, 2);
    let messages = store.list_messages(chat.id).await.unwrap();
    assert_eq!(
        messages.last().map(|message| message.content.as_str()),
        Some("here is the answer")
    );
}

#[tokio::test]
async fn post_message_rejects_blank_content_as_bad_request() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/chats/{}/messages", chat.id))
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"turn_id": TurnId::new(), "content": " \n "}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/chats/{}/messages", chat.id))
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "turn_id": uuid::Uuid::nil(),
                        "content": "valid content"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn message_to_unknown_chat_is_404() {
    let (router, token, _store, _dir) = test_app().await;
    assert_eq!(
        send_message(&router, &format!("Bearer {token}"), ChatId::new(), "hi").await,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_turn_on_the_same_chat_is_refused() {
    // A gated provider keeps the first turn active (blocked on the gate) while
    // we submit a second one, which must be refused with 409.
    let gate = Arc::new(Notify::new());
    let (router, token, _store, _dir) =
        test_app_with(Arc::new(GatedProvider { gate: gate.clone() })).await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;

    // The database's live-turn constraint owns the slot before the 202, even if
    // the worker has not claimed the queued turn yet.
    assert_eq!(
        send_message(&router, &bearer, chat.id, "one").await,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        send_message(&router, &bearer, chat.id, "two").await,
        StatusCode::CONFLICT
    );

    // Release the first turn so it can finish and free the slot.
    gate.notify_one();
}

#[tokio::test(flavor = "multi_thread")]
async fn slot_frees_after_a_turn_completes() {
    let (router, token, store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;

    assert_eq!(
        send_message(&router, &bearer, chat.id, "one").await,
        StatusCode::ACCEPTED
    );
    wait_for_turn(&store, chat.id).await;

    // The turn finished, so its slot is released and a follow-up is accepted.
    assert_eq!(
        send_message(&router, &bearer, chat.id, "two").await,
        StatusCode::ACCEPTED
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn cancellation_reports_conflict_after_completion_wins() {
    let (router, token, store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let turn_id = TurnId::new();

    assert_eq!(
        send_message_with_id(&router, &bearer, chat.id, turn_id, "finish first").await,
        StatusCode::ACCEPTED
    );
    wait_for_turn(&store, chat.id).await;
    assert_eq!(
        cancel_turn(&router, &bearer, chat.id, turn_id).await,
        StatusCode::CONFLICT
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn cancellation_after_drive_result_persists_the_completed_model_step() {
    struct AccountedOneStepProvider;

    #[async_trait]
    impl ModelProvider for AccountedOneStepProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("accounted-one-step")
        }

        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            Ok(stream::iter(vec![
                ProviderEvent::Usage(Usage {
                    input_tokens: 11,
                    output_tokens: 5,
                    ..Usage::default()
                }),
                ProviderEvent::TextDelta {
                    text: "finished before cancellation".into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ])
            .boxed())
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(Arc::new(AccountedOneStepProvider))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    let drive_returned = Arc::new(Notify::new());
    let release_outcome = Arc::new(Notify::new());
    let worker = engine::internal::leg::LegDriver::new(
        state.store.clone(),
        state.resolver.clone(),
        state.secrets.clone(),
        state.provisioned_policy.clone(),
        state.os_policy.clone(),
        state.tools.clone(),
        state.approvals.clone(),
        state.events.clone(),
        state.active_turns.clone(),
        state.turn_job_wake.clone(),
        state.agent_run_wake.clone(),
        state.queued_turn_wake.clone(),
        state.agent_config.clone(),
        None,
        engine::internal::leg::LegDriverConfig::default(),
    )
    .with_post_drive_pause(drive_returned.clone(), release_outcome.clone());
    let token = state.token.clone();
    tokio::spawn(worker.run());
    let router = app(state);
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let turn_id = TurnId::new();

    assert_eq!(
        send_message_with_id(&router, &bearer, chat.id, turn_id, "finish one step").await,
        StatusCode::ACCEPTED
    );
    tokio::time::timeout(Duration::from_secs(2), drive_returned.notified())
        .await
        .expect("the one-step drive returned before outcome handling");
    assert_eq!(
        cancel_turn(&router, &bearer, chat.id, turn_id).await,
        StatusCode::ACCEPTED
    );
    release_outcome.notify_one();

    let expected_usage = Usage {
        input_tokens: 11,
        output_tokens: 5,
        ..Usage::default()
    };
    let events = wait_for_turn(&store, chat.id).await;
    assert!(matches!(
        events.last().map(|event| &event.event),
        Some(AgentEvent::TurnCancelled { usage }) if *usage == expected_usage
    ));
    let turn = store
        .get_turn(turn_id)
        .await
        .unwrap()
        .expect("cancelled turn remains queryable");
    assert_eq!(turn.status, TurnRunStatus::Cancelled);
    assert_eq!(turn.model_steps, 1);
    assert_eq!(turn.usage, expected_usage);
}

#[tokio::test(flavor = "multi_thread")]
async fn cancellation_during_stream_persists_the_started_model_step() {
    struct StreamingUntilCancelledProvider;

    #[async_trait]
    impl ModelProvider for StreamingUntilCancelledProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("streaming-until-cancelled")
        }

        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            Ok(stream::iter(vec![
                ProviderEvent::Usage(Usage {
                    input_tokens: 13,
                    output_tokens: 4,
                    ..Usage::default()
                }),
                ProviderEvent::TextDelta {
                    text: "partial before cancellation".into(),
                },
            ])
            .chain(stream::pending())
            .boxed())
        }
    }

    let (router, token, store, _dir) =
        test_app_with(Arc::new(StreamingUntilCancelledProvider)).await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let turn_id = TurnId::new();

    assert_eq!(
        send_message_with_id(&router, &bearer, chat.id, turn_id, "stream one step").await,
        StatusCode::ACCEPTED
    );
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if store
                .list_events(chat.id, 0)
                .await
                .unwrap()
                .iter()
                .any(|event| {
                    matches!(
                        &event.event,
                        AgentEvent::TextDelta { text }
                            if text == "partial before cancellation"
                    )
                })
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the first provider delta was durably journaled");

    assert_eq!(
        cancel_turn(&router, &bearer, chat.id, turn_id).await,
        StatusCode::ACCEPTED
    );

    let expected_usage = Usage {
        input_tokens: 13,
        output_tokens: 4,
        ..Usage::default()
    };
    let events = wait_for_turn(&store, chat.id).await;
    assert!(matches!(
        events.last().map(|event| &event.event),
        Some(AgentEvent::TurnCancelled { usage }) if *usage == expected_usage
    ));
    let turn = store
        .get_turn(turn_id)
        .await
        .unwrap()
        .expect("cancelled turn remains queryable");
    assert_eq!(turn.status, TurnRunStatus::Cancelled);
    assert_eq!(turn.model_steps, 1);
    assert_eq!(turn.usage, expected_usage);
    assert!(store
        .list_messages(chat.id)
        .await
        .unwrap()
        .iter()
        .any(|message| {
            message.role == Role::Assistant && message.content == "partial before cancellation"
        }));
}

#[tokio::test(flavor = "multi_thread")]
async fn durable_slot_stays_held_and_cancel_can_win_terminal_commit_race() {
    let dir = tempfile::tempdir().unwrap();
    let inner: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let store: Arc<dyn Store> = Arc::new(PauseTerminalStore::new(
        inner,
        entered.clone(),
        release.clone(),
    ));
    let state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(Arc::new(FakeProvider))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    let token = state.token.clone();
    spawn_turn_worker(&state);
    let router = app(state);
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;

    let turn_id = TurnId::new();
    assert_eq!(
        send_message_with_id(&router, &bearer, chat.id, turn_id, "one").await,
        StatusCode::ACCEPTED
    );
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .expect("turn reached blocked atomic terminal commit");

    assert_eq!(
        send_message(&router, &bearer, chat.id, "two").await,
        StatusCode::CONFLICT,
        "durable slot must remain held until the terminal transition commits"
    );
    assert_eq!(
        steer_turn(&router, &bearer, chat.id, turn_id, "late", false).await,
        StatusCode::ACCEPTED,
        "a live durable turn must accept steering even while completion is in flight"
    );
    assert_eq!(
        cancel_turn(&router, &bearer, chat.id, turn_id).await,
        StatusCode::ACCEPTED,
        "durable cancellation may win until completion commits"
    );

    release.notify_one();
    let events = wait_for_turn(&store, chat.id).await;
    assert!(matches!(
        events.last().map(|event| &event.event),
        Some(AgentEvent::TurnCancelled { .. })
    ));
    let messages = store.list_messages(chat.id).await.unwrap();
    assert_eq!(
        messages
            .iter()
            .map(|message| (message.role, message.content.as_str()))
            .collect::<Vec<_>>(),
        vec![(tidebreak_core::Role::User, "one")],
        "cancellation rejects the pending steer without an orphan assistant candidate"
    );
    assert_eq!(
        send_message(&router, &bearer, chat.id, "two").await,
        StatusCode::ACCEPTED
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn steer_wins_a_completion_race_and_restarts_generation() {
    struct FinishTwice {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ModelProvider for FinishTwice {
        fn id(&self) -> ProviderId {
            ProviderId::new("finish-twice")
        }

        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            let text = if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                "before steer"
            } else {
                "after steer"
            };
            Ok(stream::iter(vec![
                ProviderEvent::TextDelta { text: text.into() },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ])
            .boxed())
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let inner: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let store: Arc<dyn Store> = Arc::new(PauseTerminalStore::new(
        inner,
        entered.clone(),
        release.clone(),
    ));
    let calls = Arc::new(AtomicUsize::new(0));
    let state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(Arc::new(FinishTwice {
            calls: calls.clone(),
        }))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    let token = state.token.clone();
    spawn_turn_worker(&state);
    let router = app(state);
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let turn_id = TurnId::new();

    assert_eq!(
        send_message_with_id(&router, &bearer, chat.id, turn_id, "go").await,
        StatusCode::ACCEPTED
    );
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .expect("first generation reached its atomic completion");

    let steer_id = TurnSteerId::new();
    assert_eq!(
        steer_turn_with_id(
            &router,
            &bearer,
            chat.id,
            steer_id,
            turn_id,
            "replace the answer",
            false,
        )
        .await,
        StatusCode::ACCEPTED
    );
    release.notify_one();

    let events = wait_for_turn(&store, chat.id).await;
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let interrupted_at = events
        .iter()
        .position(|event| matches!(event.event, AgentEvent::StreamInterrupted))
        .expect("superseded output clears already-streamed deltas");
    let steered_at = events
        .iter()
        .position(|event| {
            matches!(
                &event.event,
                AgentEvent::UserSteered { content, .. } if content == "replace the answer"
            )
        })
        .expect("the next generation applies the durable steer");
    assert!(interrupted_at < steered_at);
    assert!(matches!(
        events.last().map(|event| &event.event),
        Some(AgentEvent::TurnCompleted { .. })
    ));

    let messages = store.list_messages(chat.id).await.unwrap();
    assert_eq!(
        messages
            .iter()
            .map(|message| (message.id, message.role, message.content.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (messages[0].id, tidebreak_core::Role::User, "go"),
            (
                tidebreak_core::MessageId(steer_id.0),
                tidebreak_core::Role::User,
                "replace the answer",
            ),
            (
                messages[2].id,
                tidebreak_core::Role::Assistant,
                "after steer"
            ),
        ]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn late_steers_share_the_turn_wide_model_step_budget() {
    struct CountedFinish {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ModelProvider for CountedFinish {
        fn id(&self) -> ProviderId {
            ProviderId::new("counted-finish")
        }

        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(stream::iter(vec![
                ProviderEvent::TextDelta {
                    text: "candidate".into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ])
            .boxed())
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let inner: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let store: Arc<dyn Store> = Arc::new(PauseTerminalStore::new(
        inner,
        entered.clone(),
        release.clone(),
    ));
    let calls = Arc::new(AtomicUsize::new(0));
    let state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(Arc::new(CountedFinish {
            calls: calls.clone(),
        }))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        AgentConfig {
            model: "fake".into(),
            max_steps: 1,
            ..AgentConfig::default()
        },
    );
    let token = state.token.clone();
    spawn_turn_worker(&state);
    let router = app(state);
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let turn_id = TurnId::new();
    assert_eq!(
        send_message_with_id(&router, &bearer, chat.id, turn_id, "go").await,
        StatusCode::ACCEPTED
    );
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .expect("model output reached atomic completion");
    assert_eq!(
        steer_turn(
            &router,
            &bearer,
            chat.id,
            turn_id,
            "too late for another call",
            false,
        )
        .await,
        StatusCode::ACCEPTED
    );
    release.notify_one();

    let events = wait_for_turn(&store, chat.id).await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(matches!(
        events.last().map(|event| &event.event),
        Some(AgentEvent::TurnFailed { error }) if error.kind == "max_steps_exceeded"
    ));
    assert_eq!(
        store
            .list_messages(chat.id)
            .await
            .unwrap()
            .iter()
            .map(|message| (message.role, message.content.as_str()))
            .collect::<Vec<_>>(),
        vec![(tidebreak_core::Role::User, "go")]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn turn_fails_closed_with_no_provider_configured() {
    // The unconfigured provider errors without any network call; the turn must
    // end in TurnFailed, not hang or egress.
    let (router, token, store, _dir) =
        test_app_with(Arc::new(crate::provider::UnconfiguredProvider)).await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;

    assert_eq!(
        send_message(&router, &bearer, chat.id, "hello").await,
        StatusCode::ACCEPTED
    );
    let events = wait_for_turn(&store, chat.id).await;
    assert!(matches!(
        events.last().unwrap().event,
        AgentEvent::TurnFailed { .. }
    ));
}

/// Sensitive tool that records whether it ran.
struct SensitiveProbe {
    ran: Arc<std::sync::atomic::AtomicUsize>,
    name: &'static str,
}

#[async_trait]
impl Tool for SensitiveProbe {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name.into(),
            description: "sensitive probe".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }
    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::Sensitive
    }
    async fn execute(
        &self,
        _ctx: &ToolCtx,
        _args: serde_json::Value,
    ) -> tidebreak_core::Result<ToolOutput> {
        self.ran.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(ToolOutput::text("probed"))
    }
}

/// Provider that asks for `probe` once, then finishes.
struct ProbeProvider {
    calls: Arc<std::sync::atomic::AtomicUsize>,
    tool_name: &'static str,
}

#[async_trait]
impl ModelProvider for ProbeProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("probe")
    }
    async fn stream(
        &self,
        _req: ChatRequest,
    ) -> tidebreak_core::Result<BoxStream<'static, ProviderEvent>> {
        let events = if self
            .calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            .is_multiple_of(2)
        {
            vec![
                ProviderEvent::ToolCallStarted {
                    index: 0,
                    id: "call_probe".into(),
                    name: self.tool_name.into(),
                },
                ProviderEvent::ToolCallArgsDelta {
                    index: 0,
                    fragment: "{}".into(),
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

#[tokio::test(flavor = "multi_thread")]
async fn approval_endpoint_unparks_a_sensitive_tool() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let ran = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let tools = Arc::new(ToolRegistry::new().with(Box::new(SensitiveProbe {
        ran: ran.clone(),
        name: "search",
    })));
    let state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(Arc::new(ProbeProvider {
            calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            tool_name: "search",
        }))),
        Arc::new(MemSecrets::default()),
        tools,
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    let token = state.token.clone();
    spawn_turn_worker(&state);
    let router = app(state);
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;

    assert_eq!(
        send_message(&router, &bearer, chat.id, "probe it").await,
        StatusCode::ACCEPTED
    );

    // Wait until the turn parks on ApprovalRequired.
    let call_id = {
        let mut found = None;
        for _ in 0..200 {
            let events = store.list_events(chat.id, 0).await.unwrap();
            if let Some(id) = events.iter().find_map(|e| match &e.event {
                AgentEvent::ApprovalRequired { call_id, .. } => Some(*call_id),
                _ => None,
            }) {
                found = Some(id);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        found.expect("turn should park on ApprovalRequired")
    };
    assert_eq!(ran.load(std::sync::atomic::Ordering::SeqCst), 0);

    // Closed request schema and reason validation fail before the durable
    // decision, leaving the exact approval actionable.
    for body in [
        serde_json::json!({"decision": "reject", "reason": "bad\0reason"}),
        serde_json::json!({"decision": "reject", "grant": "whole_tool"}),
        serde_json::json!({"decision": "approve", "unexpected": true}),
    ] {
        let invalid = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/chats/{}/approvals/{call_id}", chat.id))
                    .header(header::AUTHORIZATION, &bearer)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
    }
    assert_eq!(
        store
            .list_pending_tool_call_approvals(chat.id, 100)
            .await
            .unwrap()
            .len(),
        1
    );

    // Approve via the HTTP endpoint.
    let decide = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/chats/{}/approvals/{call_id}", chat.id))
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"decision": "approve", "grant": "whole_tool"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(decide.status(), StatusCode::NO_CONTENT);

    let events = wait_for_turn(&store, chat.id).await;
    assert!(events
        .iter()
        .any(|e| matches!(e.event, AgentEvent::ApprovalDecided { approved: true, .. })));
    assert_eq!(ran.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(matches!(
        events.last().unwrap().event,
        AgentEvent::TurnCompleted { .. }
    ));

    // An exact decision retry remains an idempotent success after execution.
    let again = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/chats/{}/approvals/{call_id}", chat.id))
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"decision": "approve"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(again.status(), StatusCode::NO_CONTENT);

    // A later matching call in this chat runs under the standing grant and
    // never emits a second approval prompt.
    let second_turn = TurnId::new();
    assert_eq!(
        send_message_with_id(&router, &bearer, chat.id, second_turn, "probe it again").await,
        StatusCode::ACCEPTED
    );
    let mut completed_events = None;
    for _ in 0..500 {
        let events = store.list_events(chat.id, 0).await.unwrap();
        if events
            .iter()
            .filter(|event| matches!(event.event, AgentEvent::TurnCompleted { .. }))
            .count()
            == 2
        {
            completed_events = Some(events);
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let events = completed_events.expect("second turn should complete under the standing grant");
    assert_eq!(ran.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.event, AgentEvent::ApprovalRequired { .. }))
            .count(),
        1,
    );
}

struct FixedWebSearchResolver {
    provider: Arc<dyn WebSearchProvider>,
}

#[async_trait]
impl WebSearchResolver for FixedWebSearchResolver {
    async fn resolve(
        &self,
        _chat: Option<tidebreak_core::ChatId>,
    ) -> Result<Option<Arc<dyn WebSearchProvider>>, WebSearchResolverError> {
        Ok(Some(self.provider.clone()))
    }
}

struct RecordingWebSearchProvider {
    requests: Arc<std::sync::Mutex<Vec<WebSearchRequest>>>,
}

#[async_trait]
impl WebSearchProvider for RecordingWebSearchProvider {
    fn kind(&self) -> WebSearchProviderKind {
        WebSearchProviderKind::Exa
    }

    async fn search(&self, request: WebSearchRequest) -> Result<WebSearchResponse, WebSearchError> {
        self.requests.lock().unwrap().push(request);
        Ok(WebSearchResponse::new(
            WebSearchProviderKind::Exa,
            vec![WebSearchResult::new(
                "https://example.com/tidebreak",
                "Tidebreak release",
                "Tidebreak web search is ready.",
                None,
                Some(0.99),
                None,
                None,
                std::collections::BTreeMap::new(),
            )?],
        ))
    }
}

struct WebSearchFlowModelProvider {
    calls: AtomicUsize,
}

#[async_trait]
impl ModelProvider for WebSearchFlowModelProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("web-search-flow")
    }

    async fn stream(
        &self,
        request: ChatRequest,
    ) -> tidebreak_core::Result<BoxStream<'static, ProviderEvent>> {
        let events = match self.calls.fetch_add(1, Ordering::SeqCst) {
            0 => {
                let spec = request
                    .tools
                    .iter()
                    .find(|tool| tool.name == "web_search")
                    .expect("foreground agent must advertise web_search");
                assert_eq!(
                    spec.input_schema["properties"]["query"]["maxLength"],
                    crate::web_search::MAX_QUERY_CHARS
                );
                assert_eq!(spec.input_schema["additionalProperties"], false);
                vec![
                    ProviderEvent::ToolCallStarted {
                        index: 0,
                        id: "call_web_search".into(),
                        name: "web_search".into(),
                    },
                    ProviderEvent::ToolCallArgsDelta {
                        index: 0,
                        fragment: serde_json::json!({
                            "query": " Tidebreak release ",
                            "max_results": 3,
                            "domains": ["Example.com"],
                        })
                        .to_string(),
                    },
                    ProviderEvent::Stop {
                        reason: StopReason::ToolUse,
                    },
                ]
            }
            1 => {
                let result = request
                    .messages
                    .iter()
                    .flat_map(|message| &message.content)
                    .find_map(|block| match block {
                        ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            is_error,
                        } if tool_use_id == "call_web_search" => Some((content, is_error)),
                        _ => None,
                    })
                    .expect("normalized web-search result must return to the model");
                assert!(!result.1);
                assert_eq!(
                    serde_json::from_str::<serde_json::Value>(result.0).unwrap(),
                    serde_json::json!({
                        "provider": "exa",
                        "results": [{
                            "url": "https://example.com/tidebreak",
                            "title": "Tidebreak release",
                            "snippet": "Tidebreak web search is ready.",
                            "score": 0.99,
                        }],
                    })
                );
                vec![
                    ProviderEvent::TextDelta {
                        text: "Ready. [Tidebreak release](https://example.com/tidebreak)".into(),
                    },
                    ProviderEvent::Stop {
                        reason: StopReason::EndTurn,
                    },
                ]
            }
            call => panic!("unexpected model call {call}"),
        };
        Ok(stream::iter(events).boxed())
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn foreground_web_search_runs_end_to_end_through_durable_approval() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let provider = Arc::new(RecordingWebSearchProvider {
        requests: requests.clone(),
    });
    let tools = Arc::new(
        ToolRegistry::new().with(Box::new(WebSearchTool::new(Arc::new(
            FixedWebSearchResolver { provider },
        )))),
    );
    let state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(Arc::new(WebSearchFlowModelProvider {
            calls: AtomicUsize::new(0),
        }))),
        Arc::new(MemSecrets::default()),
        tools,
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    let token = state.token.clone();
    spawn_turn_worker(&state);
    let router = app(state);
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;

    assert_eq!(
        send_message(&router, &bearer, chat.id, "Is web search ready?").await,
        StatusCode::ACCEPTED
    );

    let (call_id, approval_kind) = {
        let mut found = None;
        for _ in 0..200 {
            let events = store.list_events(chat.id, 0).await.unwrap();
            if let Some(approval) = events.iter().find_map(|event| match &event.event {
                AgentEvent::ApprovalRequired {
                    call_id,
                    tool_name,
                    kind,
                    ..
                } if tool_name == "web_search" => Some((*call_id, *kind)),
                _ => None,
            }) {
                found = Some(approval);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        found.expect("web search should park on durable approval")
    };
    assert_eq!(
        approval_kind,
        tidebreak_core::ToolApprovalKind::WebSearchMayShareQuery
    );
    assert!(
        requests.lock().unwrap().is_empty(),
        "provider egress must not occur before approval"
    );

    let pending = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/chats/{}/approvals", chat.id))
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(pending.status(), StatusCode::OK);
    let pending: serde_json::Value = json_body(pending).await;
    assert_eq!(pending[0]["action"], "web_search");
    assert_eq!(pending[0]["approval"], "web_search_may_share_query");
    assert_eq!(pending[0]["class"], "sensitive");
    assert_eq!(pending[0]["can_approve"], true);
    assert_eq!(pending[0]["can_remember"], true);
    // Web search consents to sending a query off the device, so recovery has to
    // carry that query — a card asking about a search it cannot show is asking
    // about nothing in particular.
    assert_eq!(pending[0]["preview"]["tool"], "web_search");
    assert_eq!(pending[0]["preview"]["query"], "Tidebreak release");
    // The domain filter is told to the provider alongside the query, so it is
    // part of what the card has to show. A grant may only be built from a call
    // the card showed in full.
    assert_eq!(pending[0]["preview"]["domains"][0], "Example.com");
    let pending_json = pending.to_string();
    // What came *back* is still private: the query is the action under review,
    // the results are the answer the model works from. The snippet is the only
    // token unique to a result — the stub's title repeats the query and its URL
    // repeats the domain filter, so neither would notice a leak.
    assert!(!pending_json.contains("Tidebreak web search is ready."));
    assert!(!pending_json.contains("example.com/tidebreak"));

    let decide = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/chats/{}/approvals/{call_id}", chat.id))
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"decision": "approve"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(decide.status(), StatusCode::NO_CONTENT);

    let events = wait_for_turn(&store, chat.id).await;
    assert!(events.iter().any(|event| matches!(
        event.event,
        AgentEvent::ApprovalDecided { approved: true, .. }
    )));
    assert!(matches!(
        events.last().map(|event| &event.event),
        Some(AgentEvent::TurnCompleted { .. })
    ));

    {
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].query, "Tidebreak release");
        assert_eq!(requests[0].max_results, 3);
        assert_eq!(requests[0].domains[0].as_str(), "example.com");
    }

    let messages = store.list_messages(chat.id).await.unwrap();
    assert!(messages.iter().any(|message| {
        message.role == Role::Assistant
            && message.content == "Ready. [Tidebreak release](https://example.com/tidebreak)"
    }));
}

/// End-to-end integration test that drives the whole product path as a single
/// flow over one in-process server and store: synchronous raw document ingest
/// and a chat turn whose Sensitive tool crosses the approval
/// gate — first parking on the gate, then reusing a standing grant so a later
/// covered call runs without re-prompting.
///
/// Unlike the per-slice tests, the approval store the gate parks in is the one
/// the HTTP endpoint decides against. The point is to catch gaps the mocked-seam
/// unit tests cannot.
#[tokio::test(flavor = "multi_thread")]
async fn ingest_then_chat_through_the_approval_gate() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let ran = Arc::new(AtomicUsize::new(0));
    // Named "search": the approval calibration only presents recognized tools,
    // so this is the one Sensitive name the approval endpoint can approve (an
    // arbitrary name is only rejectable — see the unpresentable-tool test).
    let tools = Arc::new(ToolRegistry::new().with(Box::new(SensitiveProbe {
        ran: ran.clone(),
        name: "search",
    })));
    let state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(Arc::new(ProbeProvider {
            calls: Arc::new(AtomicUsize::new(0)),
            tool_name: "search",
        }))),
        Arc::new(MemSecrets::default()),
        tools,
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    let token = state.token.clone();
    spawn_turn_worker(&state);
    let router = app(state);
    let bearer = format!("Bearer {token}");

    // 1. Ingest a raw document through the server's document route.
    let raw = b"Jupiter is the largest planet in the Solar System, a gas giant.".to_vec();
    let response = post_raw(
        &router,
        &bearer,
        "/documents/raw?uri=file%3A%2F%2F%2Fsolar.txt",
        Some("text/plain; charset=utf-8"),
        raw.clone(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let accepted: serde_json::Value = json_body(response).await;
    let document_id: tidebreak_core::DocumentId =
        accepted["document_id"].as_str().unwrap().parse().unwrap();

    // 2. The response only returns after canonical text is stored.
    let ready = store.get_document(document_id).await.unwrap().unwrap();
    assert_eq!(ready.canonical_text, String::from_utf8_lossy(&raw));

    // 3a. A chat turn calls the Sensitive tool and parks on the approval gate.
    let chat = make_chat(&router, &bearer).await;
    let first_turn = TurnId::new();
    assert_eq!(
        send_message_with_id(&router, &bearer, chat.id, first_turn, "search the library").await,
        StatusCode::ACCEPTED
    );
    let call_id = {
        let mut found = None;
        for _ in 0..500 {
            let events = store.list_events(chat.id, 0).await.unwrap();
            if let Some(id) = events.iter().find_map(|e| match &e.event {
                AgentEvent::ApprovalRequired { call_id, .. } => Some(*call_id),
                _ => None,
            }) {
                found = Some(id);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        found.expect("first turn should park on ApprovalRequired")
    };
    // The gate holds the call: the Sensitive tool has not executed yet.
    assert_eq!(ran.load(Ordering::SeqCst), 0);

    // Approve through the HTTP endpoint, remembering the decision.
    let decide = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/chats/{}/approvals/{call_id}", chat.id))
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"decision": "approve", "grant": "whole_tool"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(decide.status(), StatusCode::NO_CONTENT);

    let events = wait_for_turn(&store, chat.id).await;
    assert!(events
        .iter()
        .any(|e| matches!(e.event, AgentEvent::ApprovalDecided { approved: true, .. })));
    assert!(matches!(
        events.last().unwrap().event,
        AgentEvent::TurnCompleted { .. }
    ));
    assert_eq!(ran.load(Ordering::SeqCst), 1);

    // 3b. A second covered call runs under the standing grant left by
    // `remember: true` — no second approval prompt, the tool just executes.
    let second_turn = TurnId::new();
    assert_eq!(
        send_message_with_id(&router, &bearer, chat.id, second_turn, "search again").await,
        StatusCode::ACCEPTED
    );
    let mut completed = None;
    for _ in 0..500 {
        let events = store.list_events(chat.id, 0).await.unwrap();
        if events
            .iter()
            .filter(|event| matches!(event.event, AgentEvent::TurnCompleted { .. }))
            .count()
            == 2
        {
            completed = Some(events);
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let events = completed.expect("second turn should complete under the standing grant");
    assert_eq!(ran.load(Ordering::SeqCst), 2);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.event, AgentEvent::ApprovalRequired { .. }))
            .count(),
        1,
        "the standing grant must suppress a second approval prompt",
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn approval_endpoint_rejects_unpresentable_sensitive_tool_approval() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let ran = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let tool_name = "third_party_sensitive";
    let tools = Arc::new(ToolRegistry::new().with(Box::new(SensitiveProbe {
        ran: ran.clone(),
        name: tool_name,
    })));
    let state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(Arc::new(ProbeProvider {
            calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            tool_name,
        }))),
        Arc::new(MemSecrets::default()),
        tools,
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    let token = state.token.clone();
    spawn_turn_worker(&state);
    let router = app(state);
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;

    assert_eq!(
        send_message(&router, &bearer, chat.id, "run it").await,
        StatusCode::ACCEPTED
    );
    let call_id = {
        let mut found = None;
        for _ in 0..200 {
            let events = store.list_events(chat.id, 0).await.unwrap();
            if let Some(id) = events.iter().find_map(|event| match &event.event {
                AgentEvent::ApprovalRequired { call_id, .. } => Some(*call_id),
                _ => None,
            }) {
                found = Some(id);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        found.expect("turn should park on ApprovalRequired")
    };

    let approve = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/chats/{}/approvals/{call_id}", chat.id))
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"decision": "approve"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(approve.status(), StatusCode::CONFLICT);
    let info: AgentErrorInfo = json_body(approve).await;
    assert_eq!(info.kind, "approval_action_not_presentable");
    assert_eq!(ran.load(std::sync::atomic::Ordering::SeqCst), 0);

    let reject = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/chats/{}/approvals/{call_id}", chat.id))
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"decision": "reject"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(reject.status(), StatusCode::NO_CONTENT);

    let events = wait_for_turn(&store, chat.id).await;
    assert!(events.iter().any(|event| matches!(
        event.event,
        AgentEvent::ApprovalDecided {
            approved: false,
            ..
        }
    )));
    assert_eq!(ran.load(std::sync::atomic::Ordering::SeqCst), 0);
}
