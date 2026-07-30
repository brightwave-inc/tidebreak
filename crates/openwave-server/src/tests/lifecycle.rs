use super::*;

#[tokio::test]
async fn cancel_stops_a_running_turn() {
    // A turn that blocks in the provider (a stand-in for a long model call),
    // so it stays running until we cancel it.
    let gate = Arc::new(Notify::new());
    let (router, token, store, _dir) =
        test_app_with(Arc::new(GatedProvider { gate: gate.clone() })).await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let turn_id = TurnId::new();

    assert_eq!(
        send_message_with_id(&router, &bearer, chat.id, turn_id, "go").await,
        StatusCode::ACCEPTED
    );

    // Acceptance is durable before 202, so cancellation works whether the
    // asynchronous worker still sees queued work or already holds its lease.
    let cancel_status = cancel_turn(&router, &bearer, chat.id, turn_id).await;
    assert_eq!(
        cancel_status,
        StatusCode::ACCEPTED,
        "turn after cancel response: {:?}",
        store.get_turn_run(turn_id).await.unwrap()
    );

    // The turn preempts the blocked provider call and ends as cancelled —
    // note we never release `gate`, so only the cancel can end it.
    let events = wait_for_turn(&store, chat.id).await;
    assert!(matches!(
        events.last().map(|e| &e.event),
        Some(AgentEvent::TurnCancelled { .. })
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn cancellation_drains_buffered_preassigned_event_ordinals() {
    struct TwoDeltasThenPark {
        second_yielded: Arc<Notify>,
    }

    #[async_trait]
    impl ModelProvider for TwoDeltasThenPark {
        fn id(&self) -> ProviderId {
            ProviderId::new("two-deltas-then-park")
        }

        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            let second_yielded = self.second_yielded.clone();
            Ok(stream::iter(vec![ProviderEvent::TextDelta {
                text: "first".into(),
            }])
            .chain(stream::once(async move {
                second_yielded.notify_one();
                ProviderEvent::TextDelta {
                    text: "second".into(),
                }
            }))
            .chain(stream::pending())
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
    let append_entered = Arc::new(Notify::new());
    let append_release = Arc::new(Notify::new());
    let injected = Arc::new(PauseTerminalStore::new(
        inner,
        append_entered.clone(),
        append_release.clone(),
    ));
    injected.do_not_pause_terminal();
    injected.pause_next_nonterminal_event();
    let store: Arc<dyn Store> = injected;
    let second_yielded = Arc::new(Notify::new());
    let state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(Arc::new(TwoDeltasThenPark {
            second_yielded: second_yielded.clone(),
        }))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    let token = state.token.clone();
    spawn_turn_worker_with_config(
        &state,
        turn_worker::TurnWorkerConfig {
            max_concurrency: 1,
            ..turn_worker::TurnWorkerConfig::default()
        },
    );
    let router = app(state);
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let turn_id = TurnId::new();
    let append_blocked = append_entered.notified();
    assert_eq!(
        send_message_with_id(&router, &bearer, chat.id, turn_id, "go").await,
        StatusCode::ACCEPTED
    );
    tokio::time::timeout(Duration::from_secs(2), append_blocked)
        .await
        .expect("worker reached the first buffered event append");
    tokio::time::timeout(Duration::from_secs(2), second_yielded.notified())
        .await
        .expect("agent yielded the following preassigned event");

    assert_eq!(
        cancel_turn(&router, &bearer, chat.id, turn_id).await,
        StatusCode::ACCEPTED
    );
    append_release.notify_one();

    let events = wait_for_turn(&store, chat.id).await;
    assert!(matches!(
        events.last().map(|event| &event.event),
        Some(AgentEvent::TurnCancelled { .. })
    ));
}

#[tokio::test]
async fn cancel_without_a_running_turn_is_a_conflict_and_unknown_chat_is_404() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;

    // Known chat, nothing running → 409.
    assert_eq!(
        cancel_turn(&router, &bearer, chat.id, TurnId::new()).await,
        StatusCode::CONFLICT
    );
    // Unknown chat → 404.
    assert_eq!(
        cancel_turn(&router, &bearer, ChatId::new(), TurnId::new()).await,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn cancel_cannot_target_a_turn_through_another_chat() {
    let gate = Arc::new(Notify::new());
    let (router, token, store, _dir) = test_app_with(Arc::new(GatedProvider { gate })).await;
    let bearer = format!("Bearer {token}");
    let owner = make_chat(&router, &bearer).await;
    let other = make_chat(&router, &bearer).await;
    let turn_id = TurnId::new();

    assert_eq!(
        send_message_with_id(&router, &bearer, owner.id, turn_id, "go").await,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        cancel_turn(&router, &bearer, other.id, turn_id).await,
        StatusCode::CONFLICT
    );
    assert_ne!(
        store.get_turn_run(turn_id).await.unwrap().unwrap().status,
        openwave_core::TurnRunStatus::Cancelled
    );
    assert_eq!(
        cancel_turn(&router, &bearer, owner.id, turn_id).await,
        StatusCode::ACCEPTED
    );
    wait_for_turn(&store, owner.id).await;
}

/// POST `/chats/{id}/steer`, returning the response status.
pub(super) async fn steer_turn(
    router: &Router,
    bearer: &str,
    chat: ChatId,
    turn_id: TurnId,
    content: &str,
    interrupt: bool,
) -> StatusCode {
    steer_turn_with_id(
        router,
        bearer,
        chat,
        TurnSteerId::new(),
        turn_id,
        content,
        interrupt,
    )
    .await
}

pub(super) async fn steer_turn_with_id(
    router: &Router,
    bearer: &str,
    chat: ChatId,
    steer_id: TurnSteerId,
    turn_id: TurnId,
    content: &str,
    interrupt: bool,
) -> StatusCode {
    router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/chats/{chat}/steer"))
                .header(header::AUTHORIZATION, bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "steer_id": steer_id,
                        "turn_id": turn_id,
                        "content": content,
                        "interrupt": interrupt
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn steer_without_a_running_turn_is_a_conflict_and_unknown_chat_is_404() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;

    assert_eq!(
        steer_turn(&router, &bearer, chat.id, TurnId::new(), "hi", false).await,
        StatusCode::CONFLICT
    );
    assert_eq!(
        steer_turn(&router, &bearer, ChatId::new(), TurnId::new(), "hi", false,).await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        steer_turn(&router, &bearer, chat.id, TurnId::new(), "  ", false).await,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        steer_turn_with_id(
            &router,
            &bearer,
            chat.id,
            TurnSteerId(uuid::Uuid::nil()),
            TurnId::new(),
            "hi",
            false,
        )
        .await,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        steer_turn(
            &router,
            &bearer,
            chat.id,
            TurnId::new(),
            "contains\0nul",
            false,
        )
        .await,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        steer_turn(
            &router,
            &bearer,
            chat.id,
            TurnId::new(),
            &"x".repeat(openwave_core::TurnSteer::MAX_CONTENT_LEN + 1),
            false,
        )
        .await,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn interrupt_steer_preempts_a_running_turn_and_continues() {
    // Stall after the first delta so steer can interrupt; then finish.
    struct StallThenFinish {
        calls: AtomicUsize,
        entered: Arc<Notify>,
    }
    #[async_trait]
    impl ModelProvider for StallThenFinish {
        fn id(&self) -> ProviderId {
            ProviderId::new("stall-then-finish")
        }
        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                self.entered.notify_one();
                let head = stream::iter(vec![ProviderEvent::TextDelta {
                    text: "partial".into(),
                }]);
                return Ok(head.chain(stream::pending()).boxed());
            }
            Ok(stream::iter(vec![
                ProviderEvent::TextDelta {
                    text: "after steer".into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ])
            .boxed())
        }
    }

    let entered = Arc::new(Notify::new());
    let (router, token, store, _dir) = test_app_with(Arc::new(StallThenFinish {
        calls: AtomicUsize::new(0),
        entered: entered.clone(),
    }))
    .await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let turn_id = TurnId::new();

    assert_eq!(
        send_message_with_id(&router, &bearer, chat.id, turn_id, "go").await,
        StatusCode::ACCEPTED
    );
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .expect("provider entered before the interrupt steer");
    let steer_id = TurnSteerId::new();
    assert_eq!(
        steer_turn_with_id(
            &router,
            &bearer,
            chat.id,
            steer_id,
            turn_id,
            "change course",
            true,
        )
        .await,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        steer_turn_with_id(
            &router,
            &bearer,
            chat.id,
            steer_id,
            turn_id,
            "change course",
            true,
        )
        .await,
        StatusCode::ACCEPTED,
        "an exact admission retry is idempotent"
    );
    assert_eq!(
        steer_turn_with_id(
            &router,
            &bearer,
            chat.id,
            steer_id,
            turn_id,
            "different request data",
            true,
        )
        .await,
        StatusCode::CONFLICT,
        "reusing an identity for different input must fail"
    );

    let events = wait_for_turn(&store, chat.id).await;
    let stream_interrupted_at = events
        .iter()
        .position(|e| matches!(e.event, AgentEvent::StreamInterrupted));
    let user_steered_at = events.iter().position(|e| {
        matches!(
            &e.event,
            AgentEvent::UserSteered { content, .. } if content == "change course"
        )
    });
    assert!(
        matches!((stream_interrupted_at, user_steered_at), (Some(a), Some(b)) if a < b),
        "interrupted stream is marked before steer is injected"
    );
    assert!(events.iter().any(|e| matches!(
        &e.event,
        AgentEvent::UserSteered { content, .. } if content == "change course"
    )));
    assert!(matches!(
        events.last().map(|e| &e.event),
        Some(AgentEvent::TurnCompleted { .. })
    ));
    let mut visible_assistant = String::new();
    for event in events.iter().map(|e| &e.event) {
        match event {
            AgentEvent::TextDelta { text } => visible_assistant.push_str(text),
            AgentEvent::StreamInterrupted => visible_assistant.clear(),
            _ => {}
        }
    }
    assert_eq!(visible_assistant, "after steer");
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.event, AgentEvent::TurnCancelled { .. })),
        "steer continues the turn"
    );
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
                "change course",
            ),
            (
                messages[2].id,
                openwave_core::Role::Assistant,
                "after steer"
            ),
        ]
    );
    assert!(matches!(
        store
            .accept_turn_steer(steer_id, turn_id, chat.id, "change course", true)
            .await
            .unwrap(),
        openwave_core::AcceptTurnSteerOutcome::Existing(openwave_core::TurnSteer {
            status: openwave_core::TurnSteerStatus::Applied,
            ..
        })
    ));
}

#[tokio::test]
async fn boundary_steer_commits_the_candidate_and_instruction_atomically() {
    struct FinishAfterGate {
        calls: Arc<AtomicUsize>,
        entered: Arc<Notify>,
        gate: Arc<Notify>,
    }

    #[async_trait]
    impl ModelProvider for FinishAfterGate {
        fn id(&self) -> ProviderId {
            ProviderId::new("finish-after-gate")
        }

        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                self.entered.notify_one();
                let gate = self.gate.clone();
                return Ok(stream::iter(vec![ProviderEvent::TextDelta {
                    text: "candidate".into(),
                }])
                .chain(stream::once(async move {
                    gate.notified().await;
                    ProviderEvent::Stop {
                        reason: StopReason::EndTurn,
                    }
                }))
                .boxed());
            }
            Ok(stream::iter(vec![
                ProviderEvent::TextDelta {
                    text: "final".into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ])
            .boxed())
        }
    }

    let calls = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(Notify::new());
    let gate = Arc::new(Notify::new());
    let (router, token, store, _dir) = test_app_with(Arc::new(FinishAfterGate {
        calls: calls.clone(),
        entered: entered.clone(),
        gate: gate.clone(),
    }))
    .await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let turn_id = TurnId::new();
    assert_eq!(
        send_message_with_id(&router, &bearer, chat.id, turn_id, "go").await,
        StatusCode::ACCEPTED
    );
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .expect("provider entered the first boundary generation");

    let steer_id = TurnSteerId::new();
    assert_eq!(
        steer_turn_with_id(
            &router,
            &bearer,
            chat.id,
            steer_id,
            turn_id,
            "continue with this",
            false,
        )
        .await,
        StatusCode::ACCEPTED
    );
    gate.notify_one();

    let events = wait_for_turn(&store, chat.id).await;
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                &event.event,
                AgentEvent::UserSteered { content, .. } if content == "continue with this"
            ))
            .count(),
        1,
        "boundary steering must publish its committed event once"
    );
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
            (messages[1].id, openwave_core::Role::Assistant, "candidate",),
            (
                openwave_core::MessageId(steer_id.0),
                openwave_core::Role::User,
                "continue with this",
            ),
            (messages[3].id, openwave_core::Role::Assistant, "final"),
        ]
    );
}

#[tokio::test]
async fn durable_steer_poll_recovers_a_missing_local_notification() {
    struct StallThenFinish {
        calls: AtomicUsize,
        entered: Arc<Notify>,
    }
    #[async_trait]
    impl ModelProvider for StallThenFinish {
        fn id(&self) -> ProviderId {
            ProviderId::new("durable-steer-poll")
        }

        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                self.entered.notify_one();
                return Ok(stream::pending().boxed());
            }
            Ok(stream::iter(vec![
                ProviderEvent::TextDelta {
                    text: "after durable poll".into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ])
            .boxed())
        }
    }

    let provider = Arc::new(StallThenFinish {
        calls: AtomicUsize::new(0),
        entered: Arc::new(Notify::new()),
    });
    let entered = provider.entered.clone();
    let (router, token, store, _dir) = test_app_with(provider.clone()).await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let turn_id = TurnId::new();
    assert_eq!(
        send_message_with_id(&router, &bearer, chat.id, turn_id, "go").await,
        StatusCode::ACCEPTED
    );
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .expect("provider entered the generation before durable admission");

    let steer_id = TurnSteerId::new();
    assert!(matches!(
        store
            .accept_turn_steer(
                steer_id,
                turn_id,
                chat.id,
                "recover from the database",
                true,
            )
            .await
            .unwrap(),
        openwave_core::AcceptTurnSteerOutcome::Accepted(_)
    ));

    let events = wait_for_turn(&store, chat.id).await;
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    assert!(events.iter().any(|event| matches!(
        &event.event,
        AgentEvent::UserSteered { content, .. } if content == "recover from the database"
    )));
    assert!(matches!(
        events.last().map(|event| &event.event),
        Some(AgentEvent::TurnCompleted { .. })
    ));
    assert!(matches!(
        store
            .accept_turn_steer(
                steer_id,
                turn_id,
                chat.id,
                "recover from the database",
                true,
            )
            .await
            .unwrap(),
        openwave_core::AcceptTurnSteerOutcome::Existing(openwave_core::TurnSteer {
            status: openwave_core::TurnSteerStatus::Applied,
            ..
        })
    ));
}

#[tokio::test]
async fn durable_steer_retries_heartbeat_races_and_ambiguous_application() {
    struct StallThenFinish {
        calls: Arc<AtomicUsize>,
        entered: Arc<Notify>,
    }

    #[async_trait]
    impl ModelProvider for StallThenFinish {
        fn id(&self) -> ProviderId {
            ProviderId::new("durable-steer-retry")
        }

        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                self.entered.notify_one();
                return Ok(stream::pending().boxed());
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

    let dir = tempfile::tempdir().unwrap();
    let inner: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let steer_read_entered = Arc::new(Notify::new());
    let release_steer_read = Arc::new(Notify::new());
    let injected = Arc::new(PauseTerminalStore::new(
        inner,
        steer_read_entered.clone(),
        release_steer_read.clone(),
    ));
    injected.do_not_pause_terminal();
    let store: Arc<dyn Store> = injected.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let provider_entered = Arc::new(Notify::new());
    let state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(Arc::new(StallThenFinish {
            calls: calls.clone(),
            entered: provider_entered.clone(),
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
    tokio::time::timeout(Duration::from_secs(2), provider_entered.notified())
        .await
        .expect("provider entered before the ambiguous application race");

    injected.pause_before_next_steer_read();
    tokio::time::timeout(Duration::from_secs(2), steer_read_entered.notified())
        .await
        .expect("steer poll paused before reading the durable queue");
    injected.advance_before_next_steer_read();
    injected.fail_after_next_apply_steer_commit();
    let steer_id = TurnSteerId::new();
    assert_eq!(
        steer_turn_with_id(
            &router,
            &bearer,
            chat.id,
            steer_id,
            turn_id,
            "recover exactly",
            true,
        )
        .await,
        StatusCode::ACCEPTED
    );
    release_steer_read.notify_one();

    let events = wait_for_turn(&store, chat.id).await;
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                &event.event,
                AgentEvent::UserSteered { content, .. } if content == "recover exactly"
            ))
            .count(),
        1,
        "ambiguous application recovery must publish its committed event once"
    );
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
                "recover exactly",
            ),
            (messages[2].id, openwave_core::Role::Assistant, "recovered"),
        ]
    );
}

#[tokio::test]
async fn committed_steer_event_recovers_when_cancellation_wins_ambiguous_response() {
    struct NeverFinish {
        entered: Arc<Notify>,
    }

    #[async_trait]
    impl ModelProvider for NeverFinish {
        fn id(&self) -> ProviderId {
            ProviderId::new("never-finish")
        }

        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            self.entered.notify_one();
            Ok(stream::pending().boxed())
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
    let injected = Arc::new(PauseTerminalStore::new(
        inner,
        Arc::new(Notify::new()),
        Arc::new(Notify::new()),
    ));
    injected.do_not_pause_terminal();
    let cancellation_committed = injected.cancel_after_next_apply_steer_commit();
    let store: Arc<dyn Store> = injected;
    let entered = Arc::new(Notify::new());
    let state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(Arc::new(NeverFinish {
            entered: entered.clone(),
        }))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    let token = state.token.clone();
    spawn_turn_worker_with_config(
        &state,
        turn_worker::TurnWorkerConfig {
            lease: Duration::from_millis(500),
            heartbeat: Duration::from_millis(20),
            steer_poll: Duration::from_millis(5),
            idle_min: Duration::from_millis(5),
            idle_cap: Duration::from_millis(20),
            failure_delay: Duration::from_millis(5),
            max_concurrency: 1,
        },
    );
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
        .expect("provider entered before the cancellation race");

    let steer_id = TurnSteerId::new();
    assert_eq!(
        steer_turn_with_id(
            &router,
            &bearer,
            chat.id,
            steer_id,
            turn_id,
            "apply before cancellation",
            true,
        )
        .await,
        StatusCode::ACCEPTED
    );
    tokio::time::timeout(Duration::from_secs(5), cancellation_committed.notified())
        .await
        .expect("steer application committed before the injected cancellation");

    let events = wait_for_turn(&store, chat.id).await;
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                &event.event,
                AgentEvent::UserSteered { content, .. } if content == "apply before cancellation"
            ))
            .count(),
        1,
        "exact recovery must publish the atomically committed steer event"
    );
    assert!(matches!(
        events.last().map(|event| &event.event),
        Some(AgentEvent::TurnCancelled { .. })
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
                "apply before cancellation",
            ),
        ]
    );
}

#[tokio::test]
async fn queued_steer_is_applied_when_the_worker_claims_the_turn() {
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
        Arc::new(FixedResolver(Arc::new(FakeProvider))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    let token = state.token.clone();
    let router = app(state.clone());
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let turn_id = TurnId::new();

    assert_eq!(
        send_message_with_id(&router, &bearer, chat.id, turn_id, "go").await,
        StatusCode::ACCEPTED
    );
    let steer_id = TurnSteerId::new();
    assert_eq!(
        steer_turn_with_id(
            &router,
            &bearer,
            chat.id,
            steer_id,
            turn_id,
            "queued direction",
            false,
        )
        .await,
        StatusCode::ACCEPTED
    );

    spawn_turn_worker(&state);
    let events = wait_for_turn(&store, chat.id).await;
    assert!(events.iter().any(|event| matches!(
        &event.event,
        AgentEvent::UserSteered { content, .. } if content == "queued direction"
    )));
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
                "queued direction",
            ),
            (messages[2].id, openwave_core::Role::Assistant, "hi"),
        ]
    );
}

/// POST a JSON body to `uri`, returning the response.
pub(super) async fn post_json(
    router: &Router,
    bearer: &str,
    uri: &str,
    body: serde_json::Value,
) -> axum::response::Response {
    router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header(header::AUTHORIZATION, bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

/// POST a JSON body through the native-only client-executor boundary.
pub(super) async fn post_native_json(
    router: &Router,
    bearer: &str,
    uri: &str,
    body: serde_json::Value,
) -> axum::response::Response {
    router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header(header::AUTHORIZATION, bearer)
                .header(
                    crate::auth::CLIENT_EXECUTOR_HEADER,
                    crate::state::TEST_CLIENT_EXECUTOR_TOKEN,
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn park_client_wait_for_route_test(
    store: &dyn Store,
    chat_id: ChatId,
    progress: TurnCheckpointProgress,
) -> (TurnId, ClientToolCallRequest) {
    let turn_id = TurnId::new();
    store
        .accept_turn(turn_id, chat_id, "fake", "native action")
        .await
        .unwrap();
    let turn_token = uuid::Uuid::new_v4();
    let claimed_at = chrono::Utc::now();
    let claimed = store
        .claim_turn_run(
            turn_token,
            claimed_at,
            claimed_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap()
        .turn
        .unwrap();
    assert_eq!(claimed.id, turn_id);
    let call = ClientToolCallRequest {
        id: CallId::new(),
        chat_id,
        turn_id,
        provider_id: "native".into(),
        name: "connect_folder".into(),
        arguments: serde_json::json!({}),
    };
    assert!(matches!(
        store
            .park_turn_for_client_tool_call(
                turn_id,
                turn_token,
                0,
                progress,
                chrono::Utc::now(),
                &call,
            )
            .await
            .unwrap()
            .unwrap(),
        ParkTurnForClientCallOutcome::Parked { .. }
    ));
    (turn_id, call)
}

async fn park_user_questions_for_route_test(
    store: &dyn Store,
    chat_id: ChatId,
) -> (TurnId, ClientToolCallRequest) {
    let turn_id = TurnId::new();
    store
        .accept_turn(turn_id, chat_id, "fake", "ask a question")
        .await
        .unwrap();
    let turn_token = uuid::Uuid::new_v4();
    let claimed_at = chrono::Utc::now();
    store
        .claim_turn_run(
            turn_token,
            claimed_at,
            claimed_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap()
        .turn
        .unwrap();
    let call = ClientToolCallRequest {
        id: CallId::new(),
        chat_id,
        turn_id,
        provider_id: "provider-question".into(),
        name: openwave_core::ASK_USER_QUESTIONS_TOOL.into(),
        arguments: serde_json::json!({
            "questions": [{
                "id": "target",
                "header": "Target",
                "question": "Where should I deploy?",
                "options": [
                    {"id": "staging", "label": "Staging", "description": "Deploy for internal verification."},
                    {"id": "production", "label": "Production", "description": "Deploy to customers."}
                ],
                "allow_free_form": true
            }]
        }),
    };
    let parked = store
        .park_turn_for_client_tool_call(
            turn_id,
            turn_token,
            0,
            test_client_checkpoint_progress(1),
            chrono::Utc::now(),
            &call,
        )
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        parked,
        ParkTurnForClientCallOutcome::Parked {
            renderer_event: Some(_),
            ..
        }
    ));
    (turn_id, call)
}

fn test_client_checkpoint_progress(model_steps: i32) -> TurnCheckpointProgress {
    TurnCheckpointProgress {
        model_steps,
        usage: Usage {
            input_tokens: 13,
            output_tokens: 8,
            cache_read_input_tokens: 5,
            cache_creation_input_tokens: 3,
        },
    }
}

async fn resolve_parked_client_call(
    store: &dyn Store,
    chat_id: ChatId,
    call: &ClientToolCallRequest,
) {
    let lease_token = uuid::Uuid::new_v4();
    let claimed_at = chrono::Utc::now();
    store
        .claim_client_tool_call(
            call.id,
            chat_id,
            uuid::Uuid::new_v4(),
            lease_token,
            claimed_at,
            claimed_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap();
    let resolved_at = chrono::Utc::now();
    let resolved = store
        .resolve_client_tool_call_and_append_event(
            call.id,
            chat_id,
            lease_token,
            resolved_at,
            &ToolCallResolution::Completed {
                result: "connected-root".into(),
            },
            resolved_at,
        )
        .await
        .unwrap();
    assert_eq!(
        resolved.outcome,
        openwave_core::ResolveToolCallOutcome::Resolved
    );
    assert!(matches!(
        resolved.turn,
        Some(turn) if turn.status == TurnRunStatus::Resuming
    ));
}

#[tokio::test]
async fn user_question_api_is_renderer_safe_exact_and_not_native_claimable() {
    let (router, token, store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let (turn_id, call) = park_user_questions_for_route_test(&*store, chat.id).await;

    let pending = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/chats/{}/questions/pending", chat.id))
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(pending.status(), StatusCode::OK);
    let pending: serde_json::Value = json_body(pending).await;
    assert_eq!(pending[0]["call_id"], call.id.to_string());
    assert_eq!(pending[0]["turn_id"], turn_id.to_string());
    assert_eq!(pending[0]["questions"][0]["id"], "target");
    let serialized = pending.to_string();
    for private in [
        "provider-question",
        "client_executor_id",
        "lease_token",
        "history_order",
        "arguments",
    ] {
        assert!(
            !serialized.contains(private),
            "renderer question projection exposed {private}"
        );
    }

    let raw = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/chats/{}/client-executions/pending/raw", chat.id))
                .header(header::AUTHORIZATION, &bearer)
                .header(
                    crate::auth::CLIENT_EXECUTOR_HEADER,
                    crate::state::TEST_CLIENT_EXECUTOR_TOKEN,
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(raw.status(), StatusCode::OK);
    assert!(json_body::<Vec<ToolCallRecord>>(raw).await.is_empty());
    let native_claim = post_native_json(
        &router,
        &bearer,
        &format!("/chats/{}/client-executions/{}/claim", chat.id, call.id),
        serde_json::json!({
            "executor_id": uuid::Uuid::new_v4(),
            "lease_token": uuid::Uuid::new_v4(),
        }),
    )
    .await;
    assert_eq!(native_claim.status(), StatusCode::CONFLICT);

    let answer_uri = format!("/chats/{}/questions/{}/answer", chat.id, call.id);
    let invalid = post_json(
        &router,
        &bearer,
        &answer_uri,
        serde_json::json!({
            "answers": [{"question_id": "target", "option_id": "unknown"}]
        }),
    )
    .await;
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
    let answer = serde_json::json!({
        "answers": [{"question_id": "target", "option_id": "staging"}]
    });
    let first = post_json(&router, &bearer, &answer_uri, answer.clone()).await;
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(
        json_body::<serde_json::Value>(first).await["disposition"],
        "answered"
    );
    let retry = post_json(&router, &bearer, &answer_uri, answer).await;
    assert_eq!(retry.status(), StatusCode::OK);
    assert_eq!(
        json_body::<serde_json::Value>(retry).await["disposition"],
        "existing"
    );
    let conflict = post_json(
        &router,
        &bearer,
        &answer_uri,
        serde_json::json!({
            "answers": [{"question_id": "target", "option_id": "production"}]
        }),
    )
    .await;
    assert_eq!(conflict.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn client_execution_api_polls_claims_heartbeats_and_resolves_idempotently() {
    let (router, token, store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let other_chat = make_chat(&router, &bearer).await;
    let proposed_call = ToolCallRecord {
        id: CallId::new(),
        chat_id: chat.id,
        turn_id: TurnId::new(),
        provider_id: "native_1".into(),
        name: "connect_folder".into(),
        arguments: serde_json::json!({"suggested_name": "Documents"}),
        execution: ToolCallExecution::Client,
        status: ToolCallStatus::Pending,
        result: None,
        result_preview: None,
        error_code: None,
        error_detail: None,
        client_executor_id: None,
        client_lease_expires_at: None,
        created_at: chrono::Utc::now(),
        resolved_at: None,
    };
    let call = match store.accept_tool_call(&proposed_call).await.unwrap() {
        openwave_core::AcceptToolCallOutcome::Accepted(call)
        | openwave_core::AcceptToolCallOutcome::Existing(call) => call,
        openwave_core::AcceptToolCallOutcome::IdentityConflict => {
            panic!("fresh client tool call identity conflicted")
        }
    };

    let pending = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/chats/{}/client-executions/pending/raw", chat.id))
                .header(header::AUTHORIZATION, &bearer)
                .header(
                    crate::auth::CLIENT_EXECUTOR_HEADER,
                    crate::state::TEST_CLIENT_EXECUTOR_TOKEN,
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(pending.status(), StatusCode::OK);
    let pending: Vec<ToolCallRecord> = json_body(pending).await;
    assert_eq!(pending, vec![call.clone()]);

    let executor_id = uuid::Uuid::new_v4();
    let lease_token = uuid::Uuid::new_v4();
    let claim_uri = format!("/chats/{}/client-executions/{}/claim", chat.id, call.id);
    let claim_body = serde_json::json!({
        "executor_id": executor_id,
        "lease_token": lease_token,
    });
    let renderer_only = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&claim_uri)
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(claim_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(renderer_only.status(), StatusCode::UNAUTHORIZED);

    let first = post_native_json(&router, &bearer, &claim_uri, claim_body.clone()).await;
    assert_eq!(first.status(), StatusCode::OK);
    let first: serde_json::Value = json_body(first).await;
    assert_eq!(first["disposition"], "claimed");
    assert_eq!(first["lease_token"], lease_token.to_string());
    assert_eq!(first["call"]["arguments"], call.arguments);

    // A lost response can be retried with the stable secret token even though
    // the server calculates a fresh proposed expiry for the second request.
    let retry = post_native_json(&router, &bearer, &claim_uri, claim_body).await;
    assert_eq!(retry.status(), StatusCode::OK);
    let retry: serde_json::Value = json_body(retry).await;
    assert_eq!(retry["disposition"], "existing");
    assert_eq!(retry["lease_token"], lease_token.to_string());

    let stolen = post_native_json(
        &router,
        &bearer,
        &claim_uri,
        serde_json::json!({
            "executor_id": executor_id,
            "lease_token": uuid::Uuid::new_v4(),
        }),
    )
    .await;
    assert_eq!(stolen.status(), StatusCode::CONFLICT);

    let pending = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/chats/{}/client-executions/pending", chat.id))
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let pending_bytes = to_bytes(pending.into_body(), usize::MAX).await.unwrap();
    assert!(
        !String::from_utf8_lossy(&pending_bytes).contains(&lease_token.to_string()),
        "authoritative polling must never disclose the secret lease token"
    );

    let wrong_chat_heartbeat = post_native_json(
        &router,
        &bearer,
        &format!(
            "/chats/{}/client-executions/{}/heartbeat",
            other_chat.id, call.id
        ),
        serde_json::json!({"lease_token": lease_token}),
    )
    .await;
    assert_eq!(wrong_chat_heartbeat.status(), StatusCode::CONFLICT);

    tokio::time::sleep(Duration::from_millis(2)).await;
    let heartbeat = post_native_json(
        &router,
        &bearer,
        &format!("/chats/{}/client-executions/{}/heartbeat", chat.id, call.id),
        serde_json::json!({"lease_token": lease_token}),
    )
    .await;
    assert_eq!(heartbeat.status(), StatusCode::OK);
    let heartbeat: serde_json::Value = json_body(heartbeat).await;
    assert_eq!(heartbeat["disposition"], "extended");

    let resolve_uri = format!("/chats/{}/client-executions/{}/resolve", chat.id, call.id);
    let resolution = serde_json::json!({
        "lease_token": lease_token,
        "resolution": {"status": "completed", "result": "folder connected"},
    });
    let wrong_chat_resolve = post_native_json(
        &router,
        &bearer,
        &format!(
            "/chats/{}/client-executions/{}/resolve",
            other_chat.id, call.id
        ),
        resolution.clone(),
    )
    .await;
    assert_eq!(wrong_chat_resolve.status(), StatusCode::CONFLICT);
    let wrong_token = post_native_json(
        &router,
        &bearer,
        &resolve_uri,
        serde_json::json!({
            "lease_token": uuid::Uuid::new_v4(),
            "resolution": {"status": "completed", "result": "folder connected"},
        }),
    )
    .await;
    assert_eq!(wrong_token.status(), StatusCode::CONFLICT);

    let resolved = post_native_json(&router, &bearer, &resolve_uri, resolution.clone()).await;
    assert_eq!(resolved.status(), StatusCode::OK);
    let resolved: serde_json::Value = json_body(resolved).await;
    assert_eq!(resolved["disposition"], "resolved");

    // Resolution time is server-owned metadata, not part of the stable command
    // identity, so an ambiguous retry converges on token + terminal payload.
    tokio::time::sleep(Duration::from_millis(2)).await;
    let retry = post_native_json(&router, &bearer, &resolve_uri, resolution).await;
    assert_eq!(retry.status(), StatusCode::OK);
    let retry: serde_json::Value = json_body(retry).await;
    assert_eq!(retry["disposition"], "existing");

    let conflicting = post_native_json(
        &router,
        &bearer,
        &resolve_uri,
        serde_json::json!({
            "lease_token": lease_token,
            "resolution": {"status": "cancelled", "result": "not connected"},
        }),
    )
    .await;
    assert_eq!(conflicting.status(), StatusCode::CONFLICT);
    assert!(store
        .list_pending_client_tool_calls(chat.id)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn renderer_pending_client_executions_are_a_closed_folder_consent_projection() {
    let (router, token, store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let request = ToolCallRecord {
        id: CallId::new(),
        chat_id: chat.id,
        turn_id: TurnId::new(),
        provider_id: "provider-secret".into(),
        name: openwave_core::REQUEST_FOLDER_ACCESS_TOOL.into(),
        arguments: serde_json::json!({
            "reason": "Read the project notes",
            "requested_capabilities": ["read_files"],
            "folder_hint": "documents",
        }),
        execution: ToolCallExecution::Client,
        status: ToolCallStatus::Pending,
        result: None,
        result_preview: None,
        error_code: None,
        error_detail: None,
        client_executor_id: None,
        client_lease_expires_at: None,
        created_at: chrono::Utc::now(),
        resolved_at: None,
    };
    let request = match store.accept_tool_call(&request).await.unwrap() {
        openwave_core::AcceptToolCallOutcome::Accepted(call)
        | openwave_core::AcceptToolCallOutcome::Existing(call) => call,
        openwave_core::AcceptToolCallOutcome::IdentityConflict => panic!("fresh call conflicted"),
    };
    let unrelated = ToolCallRecord {
        id: CallId::new(),
        chat_id: chat.id,
        turn_id: TurnId::new(),
        provider_id: "other-provider-secret".into(),
        name: "connect_folder".into(),
        arguments: serde_json::json!({"host_path": "/Users/private"}),
        execution: ToolCallExecution::Client,
        status: ToolCallStatus::Pending,
        result: None,
        result_preview: None,
        error_code: None,
        error_detail: None,
        client_executor_id: None,
        client_lease_expires_at: None,
        created_at: chrono::Utc::now(),
        resolved_at: None,
    };
    store.accept_tool_call(&unrelated).await.unwrap();
    let malformed = ToolCallRecord {
        id: CallId::new(),
        chat_id: chat.id,
        turn_id: TurnId::new(),
        provider_id: "malformed-provider-secret".into(),
        name: openwave_core::REQUEST_FOLDER_ACCESS_TOOL.into(),
        arguments: serde_json::json!({
            "reason": "Read /Users/private",
            "requested_capabilities": ["read_files"],
        }),
        execution: ToolCallExecution::Client,
        status: ToolCallStatus::Pending,
        result: None,
        result_preview: None,
        error_code: None,
        error_detail: None,
        client_executor_id: None,
        client_lease_expires_at: None,
        created_at: chrono::Utc::now(),
        resolved_at: None,
    };
    store.accept_tool_call(&malformed).await.unwrap();
    let dangerous_reasons = [
        "Read `/Users/private/report.pdf` sentinel-backtick",
        "Read [/Users/private/report.pdf] sentinel-markdown",
        "Read file:///Users/private/report.pdf sentinel-file-uri",
        r"Read `\\server\share\secret.txt` sentinel-unc",
        r"Read `C:\Users\private\secret.txt` sentinel-drive",
        "ordinary-secret-prose",
    ];
    for reason in dangerous_reasons {
        let dangerous = ToolCallRecord {
            id: CallId::new(),
            chat_id: chat.id,
            turn_id: TurnId::new(),
            provider_id: "dangerous-provider-secret".into(),
            name: openwave_core::REQUEST_FOLDER_ACCESS_TOOL.into(),
            arguments: serde_json::json!({
                "reason": reason,
                "requested_capabilities": ["read_files"],
            }),
            execution: ToolCallExecution::Client,
            status: ToolCallStatus::Pending,
            result: None,
            result_preview: None,
            error_code: None,
            error_detail: None,
            client_executor_id: None,
            client_lease_expires_at: None,
            created_at: chrono::Utc::now(),
            resolved_at: None,
        };
        store.accept_tool_call(&dangerous).await.unwrap();
    }

    let renderer = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/chats/{}/client-executions/pending", chat.id))
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(renderer.status(), StatusCode::OK);
    let renderer: serde_json::Value = json_body(renderer).await;
    let renderer_requests = renderer.as_array().unwrap();
    assert_eq!(renderer_requests.len(), dangerous_reasons.len() + 1);
    assert!(renderer_requests.iter().any(|value| {
        value["call_id"] == request.id.to_string()
            && value["turn_id"] == request.turn_id.to_string()
            && value["folder_hint"] == "documents"
            && value["claimed"] == false
    }));
    assert!(renderer_requests.iter().all(|value| {
        value["reason"]
            == "The assistant needs read access to files outside the folders connected to this conversation."
    }));
    let serialized = renderer.to_string();
    for forbidden in [
        "provider-secret",
        "other-provider-secret",
        "malformed-provider-secret",
        "dangerous-provider-secret",
        "request_folder_access",
        "connect_folder",
        "arguments",
        "chat_id",
        "provider_id",
        "client_executor_id",
        "status",
        "execution",
        "/Users/private",
        "sentinel-backtick",
        "sentinel-markdown",
        "sentinel-file-uri",
        "sentinel-unc",
        "sentinel-drive",
        "ordinary-secret-prose",
    ] {
        assert!(!serialized.contains(forbidden), "leaked {forbidden}");
    }

    let raw_uri = format!("/chats/{}/client-executions/pending/raw", chat.id);
    let renderer_raw = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(&raw_uri)
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(renderer_raw.status(), StatusCode::UNAUTHORIZED);

    let native_raw = router
        .oneshot(
            Request::builder()
                .uri(raw_uri)
                .header(header::AUTHORIZATION, &bearer)
                .header(
                    crate::auth::CLIENT_EXECUTOR_HEADER,
                    crate::state::TEST_CLIENT_EXECUTOR_TOKEN,
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(native_raw.status(), StatusCode::OK);
    let native: Vec<ToolCallRecord> = json_body(native_raw).await;
    assert_eq!(native.len(), dangerous_reasons.len() + 3);
    assert!(native.iter().any(|call| call == &request));
    assert!(native.iter().any(|call| call.name == "connect_folder"));
}

#[tokio::test]
async fn pending_chat_prompts_are_cross_chat_opaque_summaries() {
    let (router, token, store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let question_chat = make_chat(&router, &bearer).await;
    let (_question_turn_id, question_call) =
        park_user_questions_for_route_test(&*store, question_chat.id).await;
    let folder_chat = make_chat(&router, &bearer).await;
    let folder_call = ToolCallRecord {
        id: CallId::new(),
        chat_id: folder_chat.id,
        turn_id: TurnId::new(),
        provider_id: "folder-provider-secret".into(),
        name: openwave_core::REQUEST_FOLDER_ACCESS_TOOL.into(),
        arguments: serde_json::json!({
            "reason": "Read the project notes",
            "requested_capabilities": ["read_files"],
            "folder_hint": "documents",
        }),
        execution: ToolCallExecution::Client,
        status: ToolCallStatus::Pending,
        result: None,
        result_preview: None,
        error_code: None,
        error_detail: None,
        client_executor_id: None,
        client_lease_expires_at: None,
        created_at: chrono::Utc::now(),
        resolved_at: None,
    };
    let folder_call = match store.accept_tool_call(&folder_call).await.unwrap() {
        openwave_core::AcceptToolCallOutcome::Accepted(call)
        | openwave_core::AcceptToolCallOutcome::Existing(call) => call,
        openwave_core::AcceptToolCallOutcome::IdentityConflict => {
            panic!("fresh folder call identity conflicted")
        }
    };

    let response = router
        .oneshot(
            Request::builder()
                .uri("/chats/pending-prompts")
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let summaries: serde_json::Value = json_body(response).await;
    let summaries = summaries.as_array().expect("summary response is an array");
    assert_eq!(summaries.len(), 2);
    assert!(summaries.iter().any(|summary| {
        summary["chat_id"] == question_chat.id.to_string()
            && summary["question_call_ids"] == serde_json::json!([question_call.id.to_string()])
            && summary["folder_access_call_ids"] == serde_json::json!([])
    }));
    assert!(summaries.iter().any(|summary| {
        summary["chat_id"] == folder_chat.id.to_string()
            && summary["question_call_ids"] == serde_json::json!([])
            && summary["folder_access_call_ids"] == serde_json::json!([folder_call.id.to_string()])
    }));

    let serialized = serde_json::to_string(summaries).unwrap();
    for private in [
        "provider-question",
        "folder-provider-secret",
        "Where should I deploy?",
        "Read the project notes",
        "arguments",
        "turn_id",
        "client_executor_id",
        "folder_hint",
    ] {
        assert!(
            !serialized.contains(private),
            "pending-chat summary exposed {private}"
        );
    }
}

#[tokio::test]
async fn client_resolution_publishes_cancellation_and_wakes_resumable_turns() {
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
        Arc::new(FixedResolver(Arc::new(FakeProvider))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    let token = state.token.clone();
    let router = app(state.clone());
    let bearer = format!("Bearer {token}");

    let resume_chat = make_chat(&router, &bearer).await;
    let (resume_turn, resume_call) = park_client_wait_for_route_test(
        &*store,
        resume_chat.id,
        test_client_checkpoint_progress(1),
    )
    .await;
    let resume_token = uuid::Uuid::new_v4();
    let claimed_at = chrono::Utc::now();
    store
        .claim_client_tool_call(
            resume_call.id,
            resume_chat.id,
            uuid::Uuid::new_v4(),
            resume_token,
            claimed_at,
            claimed_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap();
    let resume_response = post_native_json(
        &router,
        &bearer,
        &format!(
            "/chats/{}/client-executions/{}/resolve",
            resume_chat.id, resume_call.id
        ),
        serde_json::json!({
            "lease_token": resume_token,
            "resolution": {"status": "completed", "result": "root-1"},
        }),
    )
    .await;
    assert_eq!(resume_response.status(), StatusCode::OK);
    tokio::time::timeout(Duration::from_secs(1), state.turn_job_wake.notified())
        .await
        .expect("resumable client resolution must wake the turn worker");
    assert_eq!(
        store
            .get_turn_run(resume_turn)
            .await
            .unwrap()
            .unwrap()
            .status,
        TurnRunStatus::Resuming
    );
    store
        .request_turn_cancellation(resume_turn, chrono::Utc::now())
        .await
        .unwrap();

    let cancel_chat = make_chat(&router, &bearer).await;
    let (cancel_turn, cancel_call) = park_client_wait_for_route_test(
        &*store,
        cancel_chat.id,
        test_client_checkpoint_progress(1),
    )
    .await;
    let cancel_token = uuid::Uuid::new_v4();
    let claimed_at = chrono::Utc::now();
    store
        .claim_client_tool_call(
            cancel_call.id,
            cancel_chat.id,
            uuid::Uuid::new_v4(),
            cancel_token,
            claimed_at,
            claimed_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap();
    assert!(matches!(
        store
            .request_turn_cancellation(cancel_turn, chrono::Utc::now())
            .await
            .unwrap()
            .unwrap(),
        openwave_core::RequestTurnCancellationOutcome::Requested(turn)
            if turn.status == TurnRunStatus::CancellingClient
    ));
    let mut live = state.events.subscribe(cancel_chat.id);
    let resolve_uri = format!(
        "/chats/{}/client-executions/{}/resolve",
        cancel_chat.id, cancel_call.id
    );
    let body = serde_json::json!({
        "lease_token": cancel_token,
        "resolution": {"status": "cancelled", "result": "cancelled by user"},
    });
    let cancelled = post_native_json(&router, &bearer, &resolve_uri, body.clone()).await;
    assert_eq!(cancelled.status(), StatusCode::OK);
    let first_live = tokio::time::timeout(Duration::from_secs(1), live.recv())
        .await
        .expect("client-owned cancellation must publish live")
        .unwrap();
    assert!(matches!(first_live.event, AgentEvent::TurnCancelled { .. }));

    let retry = post_native_json(&router, &bearer, &resolve_uri, body).await;
    assert_eq!(retry.status(), StatusCode::OK);
    let retry: serde_json::Value = json_body(retry).await;
    assert_eq!(retry["disposition"], "existing");
    let recovered_live = tokio::time::timeout(Duration::from_secs(1), live.recv())
        .await
        .expect("exact retry must recover the terminal publication receipt")
        .unwrap();
    assert_eq!(recovered_live, first_live);
}

#[tokio::test(flavor = "multi_thread")]
async fn resumed_worker_preserves_checkpoint_usage_and_step_budget() {
    struct CountingUsageProvider {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ModelProvider for CountingUsageProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("counting-usage")
        }

        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(stream::iter(vec![
                ProviderEvent::TextDelta {
                    text: "resumed".into(),
                },
                ProviderEvent::Usage(Usage {
                    input_tokens: 2,
                    output_tokens: 1,
                    ..Usage::default()
                }),
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
    let calls = Arc::new(AtomicUsize::new(0));
    let state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(Arc::new(CountingUsageProvider {
            calls: calls.clone(),
        }))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        AgentConfig {
            model: "fake".into(),
            max_steps: 2,
            ..AgentConfig::default()
        },
    );
    let token = state.token.clone();
    let router = app(state.clone());
    let bearer = format!("Bearer {token}");

    let completed_chat = make_chat(&router, &bearer).await;
    let completed_progress = test_client_checkpoint_progress(1);
    let (_, completed_call) =
        park_client_wait_for_route_test(&*store, completed_chat.id, completed_progress).await;
    resolve_parked_client_call(&*store, completed_chat.id, &completed_call).await;
    spawn_turn_worker(&state);
    state.turn_job_wake.notify_one();
    let completed_events = wait_for_turn(&store, completed_chat.id).await;
    assert!(matches!(
        completed_events.last().map(|event| &event.event),
        Some(AgentEvent::TurnCompleted { usage, .. })
            if *usage == Usage {
                input_tokens: completed_progress.usage.input_tokens + 2,
                output_tokens: completed_progress.usage.output_tokens + 1,
                cache_read_input_tokens: completed_progress.usage.cache_read_input_tokens,
                cache_creation_input_tokens: completed_progress.usage.cache_creation_input_tokens,
            }
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    let exhausted_chat = make_chat(&router, &bearer).await;
    let exhausted_progress = test_client_checkpoint_progress(2);
    let (_, exhausted_call) =
        park_client_wait_for_route_test(&*store, exhausted_chat.id, exhausted_progress).await;
    resolve_parked_client_call(&*store, exhausted_chat.id, &exhausted_call).await;
    state.turn_job_wake.notify_one();
    let exhausted_events = wait_for_turn(&store, exhausted_chat.id).await;
    assert!(matches!(
        exhausted_events.last().map(|event| &event.event),
        Some(AgentEvent::TurnFailed { error }) if error.kind == "max_steps_exceeded"
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn worker_checkpoints_a_client_tool_and_resumes_after_its_result() {
    struct ClientThenFinishProvider {
        requests: Arc<std::sync::Mutex<Vec<ChatRequest>>>,
    }

    #[async_trait]
    impl ModelProvider for ClientThenFinishProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("client-then-finish")
        }

        async fn stream(&self, req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            let mut requests = self.requests.lock().unwrap();
            requests.push(req);
            let events = if requests.len() == 1 {
                vec![
                    ProviderEvent::ToolCallStarted {
                        index: 0,
                        id: "native_1".into(),
                        name: "connect_folder".into(),
                    },
                    ProviderEvent::ToolCallArgsDelta {
                        index: 0,
                        fragment: r#"{"hint":"Documents"}"#.into(),
                    },
                    ProviderEvent::Usage(Usage {
                        input_tokens: 5,
                        output_tokens: 2,
                        ..Usage::default()
                    }),
                    ProviderEvent::Stop {
                        reason: StopReason::ToolUse,
                    },
                ]
            } else {
                vec![
                    ProviderEvent::TextDelta {
                        text: "folder connected".into(),
                    },
                    ProviderEvent::Usage(Usage {
                        input_tokens: 3,
                        output_tokens: 4,
                        ..Usage::default()
                    }),
                    ProviderEvent::Stop {
                        reason: StopReason::EndTurn,
                    },
                ]
            };
            drop(requests);
            Ok(stream::iter(events).boxed())
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
    let injected = Arc::new(PauseTerminalStore::new(
        inner,
        Arc::new(Notify::new()),
        Arc::new(Notify::new()),
    ));
    injected.do_not_pause_terminal();
    injected.fail_after_next_park_commit();
    let store: Arc<dyn Store> = injected;
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut tools = ToolRegistry::new();
    tools.register_client(ToolSpec {
        name: "connect_folder".into(),
        description: "Ask the desktop to connect a folder".into(),
        input_schema: serde_json::json!({"type": "object"}),
    });
    let state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(Arc::new(ClientThenFinishProvider {
            requests: requests.clone(),
        }))),
        Arc::new(MemSecrets::default()),
        Arc::new(tools),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    let token = state.token.clone();
    spawn_turn_worker(&state);
    let router = app(state.clone());
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let turn_id = TurnId::new();
    assert_eq!(
        send_message_with_id(&router, &bearer, chat.id, turn_id, "connect documents").await,
        StatusCode::ACCEPTED
    );

    let pending = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let pending = store.list_pending_client_tool_calls(chat.id).await.unwrap();
            if let Some(call) = pending.into_iter().next() {
                break call;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("worker should durably checkpoint the client tool");
    let parked = store.get_turn_run(turn_id).await.unwrap().unwrap();
    assert_eq!(parked.status, TurnRunStatus::WaitingForClient);
    assert_eq!(parked.model_steps, 1);
    assert_eq!(parked.usage.input_tokens, 5);
    assert_eq!(parked.usage.output_tokens, 2);
    assert_eq!(pending.name, "connect_folder");
    assert_eq!(pending.execution, ToolCallExecution::Client);
    assert_eq!(pending.arguments, serde_json::json!({"hint": "Documents"}));

    resolve_parked_client_call(
        &*store,
        chat.id,
        &ClientToolCallRequest {
            id: pending.id,
            chat_id: pending.chat_id,
            turn_id: pending.turn_id,
            provider_id: pending.provider_id.clone(),
            name: pending.name.clone(),
            arguments: pending.arguments.clone(),
        },
    )
    .await;
    state.turn_job_wake.notify_one();
    let events = wait_for_turn(&store, chat.id).await;
    assert!(matches!(
        events.last().map(|event| &event.event),
        Some(AgentEvent::TurnCompleted { usage, .. })
            if usage.input_tokens == 8 && usage.output_tokens == 6
    ));
    {
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[1].messages.iter().any(|message| {
            message.content.iter().any(|block| {
                matches!(
                    block,
                    openwave_core::ContentBlock::ToolUse { id, name, .. }
                        if id == "native_1" && name == "connect_folder"
                )
            })
        }));
        assert!(requests[1].messages.iter().any(|message| {
            message.content.iter().any(|block| {
                matches!(
                    block,
                    openwave_core::ContentBlock::ToolResult { tool_use_id, content, .. }
                        if tool_use_id == "native_1" && content == "connected-root"
                )
            })
        }));
    }

    let exhausted_dir = tempfile::tempdir().unwrap();
    let exhausted_store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            exhausted_dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let mut exhausted_tools = ToolRegistry::new();
    exhausted_tools.register_client(ToolSpec {
        name: "connect_folder".into(),
        description: "Ask the desktop to connect a folder".into(),
        input_schema: serde_json::json!({"type": "object"}),
    });
    let exhausted_state = AppState::new(
        Config::desktop(exhausted_dir.path()),
        exhausted_store.clone(),
        Arc::new(FixedResolver(Arc::new(ClientThenFinishProvider {
            requests: Arc::new(std::sync::Mutex::new(Vec::new())),
        }))),
        Arc::new(MemSecrets::default()),
        Arc::new(exhausted_tools),
        AgentConfig {
            model: "fake".into(),
            max_steps: 1,
            ..AgentConfig::default()
        },
    );
    let exhausted_token = exhausted_state.token.clone();
    spawn_turn_worker(&exhausted_state);
    let exhausted_router = app(exhausted_state);
    let exhausted_bearer = format!("Bearer {exhausted_token}");
    let exhausted_chat = make_chat(&exhausted_router, &exhausted_bearer).await;
    let exhausted_turn_id = TurnId::new();
    assert_eq!(
        send_message_with_id(
            &exhausted_router,
            &exhausted_bearer,
            exhausted_chat.id,
            exhausted_turn_id,
            "connect documents",
        )
        .await,
        StatusCode::ACCEPTED
    );
    let exhausted_events = wait_for_turn(&exhausted_store, exhausted_chat.id).await;
    assert!(matches!(
        exhausted_events.last().map(|event| &event.event),
        Some(AgentEvent::TurnFailed { error }) if error.kind == "max_steps_exceeded"
    ));
    let exhausted_turn = exhausted_store
        .get_turn_run(exhausted_turn_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(exhausted_turn.model_steps, 1);
    assert_eq!(exhausted_turn.usage.input_tokens, 5);
    assert_eq!(exhausted_turn.usage.output_tokens, 2);
    assert!(exhausted_store
        .list_pending_client_tool_calls(exhausted_chat.id)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn client_execution_api_reconciles_a_known_result_after_exact_lease_expiry() {
    let (router, token, store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let call = ToolCallRecord {
        id: CallId::new(),
        chat_id: chat.id,
        turn_id: TurnId::new(),
        provider_id: "native_expired".into(),
        name: "connect_folder".into(),
        arguments: serde_json::json!({}),
        execution: ToolCallExecution::Client,
        status: ToolCallStatus::Pending,
        result: None,
        result_preview: None,
        error_code: None,
        error_detail: None,
        client_executor_id: None,
        client_lease_expires_at: None,
        created_at: chrono::Utc::now(),
        resolved_at: None,
    };
    store.accept_tool_call(&call).await.unwrap();
    let lease_token = uuid::Uuid::new_v4();
    let claimed_at = chrono::Utc::now();
    assert!(matches!(
        store
            .claim_client_tool_call(
                call.id,
                chat.id,
                uuid::Uuid::new_v4(),
                lease_token,
                claimed_at,
                claimed_at + chrono::Duration::milliseconds(2),
            )
            .await
            .unwrap(),
        openwave_core::ClaimClientToolCallOutcome::Claimed(_)
    ));
    tokio::time::sleep(Duration::from_millis(5)).await;

    let resolved = post_native_json(
        &router,
        &bearer,
        &format!("/chats/{}/client-executions/{}/resolve", chat.id, call.id),
        serde_json::json!({
            "lease_token": lease_token,
            "resolution": {
                "status": "cancelled",
                "result": "folder picker was cancelled",
            },
        }),
    )
    .await;
    assert_eq!(resolved.status(), StatusCode::OK);
    let resolved: serde_json::Value = json_body(resolved).await;
    assert_eq!(resolved["disposition"], "resolved");
}

#[tokio::test]
async fn client_execution_api_validates_scope_identity_and_terminal_payloads() {
    let (router, token, store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let missing_chat = ChatId::new();
    let missing = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/chats/{missing_chat}/client-executions/pending"))
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    let call = ToolCallRecord {
        id: CallId::new(),
        chat_id: chat.id,
        turn_id: TurnId::new(),
        provider_id: "native_validation".into(),
        name: "connect_folder".into(),
        arguments: serde_json::json!({}),
        execution: ToolCallExecution::Client,
        status: ToolCallStatus::Pending,
        result: None,
        result_preview: None,
        error_code: None,
        error_detail: None,
        client_executor_id: None,
        client_lease_expires_at: None,
        created_at: chrono::Utc::now(),
        resolved_at: None,
    };
    store.accept_tool_call(&call).await.unwrap();
    let claim_uri = format!("/chats/{}/client-executions/{}/claim", chat.id, call.id);
    let nil = post_native_json(
        &router,
        &bearer,
        &claim_uri,
        serde_json::json!({
            "executor_id": uuid::Uuid::nil(),
            "lease_token": uuid::Uuid::new_v4(),
        }),
    )
    .await;
    assert_eq!(nil.status(), StatusCode::BAD_REQUEST);

    let lease_token = uuid::Uuid::new_v4();
    let claimed = post_native_json(
        &router,
        &bearer,
        &claim_uri,
        serde_json::json!({
            "executor_id": uuid::Uuid::new_v4(),
            "lease_token": lease_token,
        }),
    )
    .await;
    assert_eq!(claimed.status(), StatusCode::OK);

    let oversized = post_native_json(
        &router,
        &bearer,
        &format!("/chats/{}/client-executions/{}/resolve", chat.id, call.id),
        serde_json::json!({
            "lease_token": lease_token,
            "resolution": {
                "status": "failed",
                "result": "failure",
                "error_code": "x".repeat(ToolCallRecord::MAX_ERROR_CODE_LEN + 1),
            },
        }),
    )
    .await;
    assert_eq!(oversized.status(), StatusCode::BAD_REQUEST);
}
