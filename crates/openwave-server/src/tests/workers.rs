use super::*;

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

    let turn = store.list_turn_runs(chat.id).await.unwrap().pop().unwrap();
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
    assert_eq!(output["refusal"]["category"], "general_harms");
    assert_eq!(output["refusal"]["partial_output"], true);
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
        build_retrieval(),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    let token = state.token.clone();
    spawn_turn_worker(&state);
    let sandbox_worker = sandbox_agent_run_worker::SandboxAgentRunWorker::new(
        state.store.clone(),
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
    let parent = openwave_core::AgentRunId::foreground_for_chat(chat.id);
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

    let turn = store.list_turn_runs(chat.id).await.unwrap().pop().unwrap();
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
                .any(|tool| tool.name == openwave_core::SPAWN_SANDBOX_AGENT_TOOL)
        })
        .collect::<Vec<_>>();
    let sandbox = requests
        .iter()
        .filter(|request| {
            request
                .tools
                .iter()
                .any(|tool| tool.name == openwave_core::SANDBOX_WEB_SEARCH_TOOL)
        })
        .collect::<Vec<_>>();
    assert_eq!(foreground.len(), 5);
    assert_eq!(sandbox.len(), 2);
    assert!(foreground.iter().all(|request| request
        .tools
        .iter()
        .any(|tool| tool.name == openwave_core::WAIT_FOR_AGENTS_TOOL)));
    assert!(sandbox
        .iter()
        .all(|request| request.tools.iter().all(|tool| {
            !matches!(
                tool.name.as_str(),
                openwave_core::SPAWN_SANDBOX_AGENT_TOOL | openwave_core::WAIT_FOR_AGENTS_TOOL
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
        call.execution == openwave_core::ToolCallExecution::Orchestration
            && call.status == openwave_core::ToolCallStatus::Completed
    }));
    assert!(calls
        .iter()
        .any(|call| call.name == openwave_core::SPAWN_SANDBOX_AGENT_TOOL));
    assert!(calls
        .iter()
        .any(|call| call.name == openwave_core::WAIT_FOR_AGENTS_TOOL));
    let wait_result = calls
        .iter()
        .find(|call| call.name == openwave_core::WAIT_FOR_AGENTS_TOOL)
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
        attachment_revision: 0,
        root_attachments: Vec::new(),
        created_at: chrono::Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    assert!(matches!(
        store
            .accept_turn(turn_id, chat.id, "fake", "queued before startup")
            .await
            .unwrap(),
        openwave_core::AcceptTurnOutcome::Accepted(_)
    ));

    let retrieval = build_retrieval();
    let state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(Arc::new(FakeProvider))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        retrieval,
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
    let retrieval = build_retrieval();
    let (router, token, store, _dir) = test_app_from_parts(
        Arc::new(GatedProvider { gate: gate.clone() }),
        retrieval,
        store,
        dir,
    );
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let turn_id = TurnId::new();
    let first_router = router.clone();
    let first_bearer = bearer.clone();
    let first = tokio::spawn(async move {
        send_message_with_id(
            &first_router,
            &first_bearer,
            chat.id,
            turn_id,
            "same request",
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .expect("first request reached acceptance");

    store
        .set_setting("model", &serde_json::json!("model-after"))
        .await
        .unwrap();
    assert_eq!(
        send_message_with_id(&router, &bearer, chat.id, turn_id, "same request").await,
        StatusCode::ACCEPTED
    );
    release.notify_one();
    assert_eq!(first.await.unwrap(), StatusCode::ACCEPTED);
    assert_eq!(store.list_turn_runs(chat.id).await.unwrap().len(), 1);
    assert_eq!(store.list_messages(chat.id).await.unwrap().len(), 1);

    gate.notify_one();
    wait_for_turn(&store, chat.id).await;
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
    let retrieval = build_retrieval();
    let (router, token, store, _dir) = test_app_from_parts_with_worker_config(
        Arc::new(FakeProvider),
        retrieval,
        store,
        dir,
        turn_worker::TurnWorkerConfig {
            // Keep this failure-injection test inside the committed claim's
            // lease even when a loaded test host delays the retry task.
            lease: Duration::from_secs(5 * 60),
            failure_delay: Duration::from_millis(10),
            max_concurrency: 1,
            ..turn_worker::TurnWorkerConfig::default()
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
    let turn = store.list_turn_runs(chat.id).await.unwrap().pop().unwrap();
    assert_eq!(turn.attempt_count, 2);
    assert_eq!(turn.status, TurnRunStatus::Completed);
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
            .chain(stream::once(async move {
                entered.notify_one();
                gate.notified().await;
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                }
            }))
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

#[tokio::test(flavor = "multi_thread")]
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
        build_retrieval(),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    let worker = turn_worker::TurnWorker::new(
        state.store.clone(),
        state.resolver.clone(),
        state.secrets.clone(),
        state.os_policy.clone(),
        state.tools.clone(),
        state.approvals.clone(),
        state.events.clone(),
        state.active_turns.clone(),
        state.turn_job_wake.clone(),
        state.agent_run_wake.clone(),
        state.agent_config.clone(),
        None,
        turn_worker::TurnWorkerConfig {
            lease: Duration::from_millis(600),
            heartbeat: Duration::from_millis(200),
            steer_poll: Duration::from_millis(10),
            idle_min: Duration::from_millis(10),
            idle_cap: Duration::from_millis(20),
            failure_delay: Duration::from_millis(10),
            max_concurrency: 1,
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
    tokio::time::sleep(Duration::from_millis(450)).await;
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
    tokio::time::sleep(Duration::from_millis(250)).await;
    gate.notify_one();

    let events = wait_for_turn(&store, chat.id).await;
    assert!(matches!(
        events.last().map(|event| &event.event),
        Some(AgentEvent::TurnCompleted { .. })
    ));
}

#[tokio::test(flavor = "multi_thread")]
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
        build_retrieval(),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    let worker = turn_worker::TurnWorker::new(
        state.store.clone(),
        state.resolver.clone(),
        state.secrets.clone(),
        state.os_policy.clone(),
        state.tools.clone(),
        state.approvals.clone(),
        state.events.clone(),
        state.active_turns.clone(),
        state.turn_job_wake.clone(),
        state.agent_run_wake.clone(),
        state.agent_config.clone(),
        None,
        turn_worker::TurnWorkerConfig {
            lease: Duration::from_millis(250),
            heartbeat: Duration::from_millis(50),
            steer_poll: Duration::from_millis(10),
            idle_min: Duration::from_millis(10),
            idle_cap: Duration::from_millis(20),
            failure_delay: Duration::from_millis(10),
            max_concurrency: 1,
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
    tokio::time::sleep(Duration::from_millis(400)).await;
    release.notify_one();

    let events = wait_for_turn(&store, chat.id).await;
    assert!(
        matches!(
            events.last().map(|event| &event.event),
            Some(AgentEvent::TurnCompleted { .. })
        ),
        "unexpected journal: {events:?}; turns: {:?}",
        store.list_turn_runs(chat.id).await.unwrap()
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
        build_retrieval(),
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
        vec![(openwave_core::Role::User, "one")],
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
    let retrieval = build_retrieval();
    let state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(Arc::new(FinishTwice {
            calls: calls.clone(),
        }))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        retrieval,
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
            (messages[0].id, openwave_core::Role::User, "go"),
            (
                openwave_core::MessageId(steer_id.0),
                openwave_core::Role::User,
                "replace the answer",
            ),
            (
                messages[2].id,
                openwave_core::Role::Assistant,
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
    let retrieval = build_retrieval();
    let state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(Arc::new(CountedFinish {
            calls: calls.clone(),
        }))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        retrieval,
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
        vec![(openwave_core::Role::User, "go")]
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
    ) -> openwave_core::Result<ToolOutput> {
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
    ) -> openwave_core::Result<BoxStream<'static, ProviderEvent>> {
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
        build_retrieval(),
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
    async fn resolve(&self) -> Result<Option<Arc<dyn WebSearchProvider>>, WebSearchResolverError> {
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
                "https://example.com/openwave",
                "OpenWave release",
                "OpenWave web search is ready.",
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
    ) -> openwave_core::Result<BoxStream<'static, ProviderEvent>> {
        let events = match self.calls.fetch_add(1, Ordering::SeqCst) {
            0 => {
                let spec = request
                    .tools
                    .iter()
                    .find(|tool| tool.name == "web_search")
                    .expect("foreground agent must advertise web_search");
                assert_eq!(
                    spec.input_schema["properties"]["query"]["maxLength"],
                    openwave_web_search::MAX_QUERY_CHARS
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
                            "query": " OpenWave release ",
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
                            "url": "https://example.com/openwave",
                            "title": "OpenWave release",
                            "snippet": "OpenWave web search is ready.",
                            "score": 0.99,
                        }],
                    })
                );
                vec![
                    ProviderEvent::TextDelta {
                        text: "Ready. [OpenWave release](https://example.com/openwave)".into(),
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
        build_retrieval(),
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
        openwave_core::ToolApprovalKind::WebSearchMayShareQuery
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
    assert_eq!(pending[0]["preview"]["query"], "OpenWave release");
    // The domain filter is told to the provider alongside the query, so it is
    // part of what the card has to show. A grant may only be built from a call
    // the card showed in full.
    assert_eq!(pending[0]["preview"]["domains"][0], "Example.com");
    let pending_json = pending.to_string();
    // What came *back* is still private: the query is the action under review,
    // the results are the answer the model works from. The snippet is the only
    // token unique to a result — the stub's title repeats the query and its URL
    // repeats the domain filter, so neither would notice a leak.
    assert!(!pending_json.contains("OpenWave web search is ready."));
    assert!(!pending_json.contains("example.com/openwave"));

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
        assert_eq!(requests[0].query, "OpenWave release");
        assert_eq!(requests[0].max_results, 3);
        assert_eq!(requests[0].domains[0].as_str(), "example.com");
    }

    let messages = store.list_messages(chat.id).await.unwrap();
    assert!(messages.iter().any(|message| {
        message.role == Role::Assistant
            && message.content == "Ready. [OpenWave release](https://example.com/openwave)"
    }));
}

/// End-to-end integration test that drives the whole product path as a single
/// flow over one in-process server, store, and worker set: raw document ingest,
/// the parse worker, and a chat turn whose Sensitive tool crosses the approval
/// gate — first parking on the gate, then reusing a standing grant so a later
/// covered call runs without re-prompting.
///
/// Unlike the per-slice tests, the approval store the gate parks in is the one
/// the HTTP endpoint decides against. The point is to catch gaps the mocked-seam
/// unit tests cannot.
#[tokio::test(flavor = "multi_thread")]
async fn ingest_parse_then_chat_through_the_approval_gate() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let retrieval = build_retrieval();
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
        retrieval.clone(),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    let token = state.token.clone();
    let worker = document_worker::DocumentWorker::new(
        store.clone(),
        state.blobs.clone(),
        retrieval,
        state.document_job_wake.clone(),
        document_worker::DocumentWorkerConfig::default(),
    );
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
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let accepted: serde_json::Value = json_body(response).await;
    let document_id: openwave_core::DocumentId =
        accepted["document_id"].as_str().unwrap().parse().unwrap();

    // 2. Drive the parse worker as the per-slice tests do.
    run_parse(&worker).await;
    let ready = store.get_document(document_id).await.unwrap().unwrap();
    assert_eq!(
        ready.processing_status,
        openwave_core::DocumentProcessingStatus::Ready
    );
    assert_eq!(ready.canonical_text, String::from_utf8_lossy(&raw));

    // 4a. A chat turn calls the Sensitive tool and parks on the approval gate.
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

    // 4b. A second covered call runs under the standing grant left by
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
        build_retrieval(),
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
