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
        store.get_turn(turn_id).await.unwrap()
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
        engine::internal::leg::LegDriverConfig {
            max_concurrency: 1,
            ..engine::internal::leg::LegDriverConfig::default()
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
        store.get_turn(turn_id).await.unwrap().unwrap().status,
        tidebreak_core::TurnRunStatus::Cancelled
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
    steer_turn_with_id_and_voice(
        router, bearer, chat, steer_id, turn_id, content, interrupt, false,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn steer_turn_with_id_and_voice(
    router: &Router,
    bearer: &str,
    chat: ChatId,
    steer_id: TurnSteerId,
    turn_id: TurnId,
    content: &str,
    interrupt: bool,
    voice_input_used: bool,
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
                        "interrupt": interrupt,
                        "voice_input_used": voice_input_used,
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
            &"x".repeat(tidebreak_core::TurnSteer::MAX_CONTENT_LEN + 1),
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
        steer_turn_with_id_and_voice(
            &router,
            &bearer,
            chat.id,
            steer_id,
            turn_id,
            "change course",
            true,
            true,
        )
        .await,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        steer_turn_with_id_and_voice(
            &router,
            &bearer,
            chat.id,
            steer_id,
            turn_id,
            "change course",
            true,
            true,
        )
        .await,
        StatusCode::ACCEPTED,
        "an exact admission retry is idempotent"
    );
    assert_eq!(
        steer_turn_with_id_and_voice(
            &router,
            &bearer,
            chat.id,
            steer_id,
            turn_id,
            "different request data",
            true,
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
            (messages[0].id, tidebreak_core::Role::User, "go"),
            (
                tidebreak_core::MessageId(steer_id.0),
                tidebreak_core::Role::User,
                "change course",
            ),
            (
                messages[2].id,
                tidebreak_core::Role::Assistant,
                "after steer"
            ),
        ]
    );
    assert!(messages[1]
        .llm_content
        .as_deref()
        .is_some_and(|content| content.contains("The user dictated this message")));
    assert!(matches!(
        store
            .accept_turn_steer_with_message_context(
                steer_id,
                turn_id,
                chat.id,
                "change course",
                &[],
                true,
                true,
            )
            .await
            .unwrap(),
        tidebreak_core::AcceptTurnSteerOutcome::Existing(tidebreak_core::TurnSteer {
            status: tidebreak_core::TurnSteerStatus::Applied,
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
            (messages[0].id, tidebreak_core::Role::User, "go"),
            (messages[1].id, tidebreak_core::Role::Assistant, "candidate",),
            (
                tidebreak_core::MessageId(steer_id.0),
                tidebreak_core::Role::User,
                "continue with this",
            ),
            (messages[3].id, tidebreak_core::Role::Assistant, "final"),
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
        tidebreak_core::AcceptTurnSteerOutcome::Accepted(_)
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
        tidebreak_core::AcceptTurnSteerOutcome::Existing(tidebreak_core::TurnSteer {
            status: tidebreak_core::TurnSteerStatus::Applied,
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
            (messages[0].id, tidebreak_core::Role::User, "go"),
            (
                tidebreak_core::MessageId(steer_id.0),
                tidebreak_core::Role::User,
                "recover exactly",
            ),
            (messages[2].id, tidebreak_core::Role::Assistant, "recovered"),
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
        engine::internal::leg::LegDriverConfig {
            lease: Duration::from_millis(500),
            heartbeat: Duration::from_millis(20),
            steer_poll: Duration::from_millis(5),
            idle_min: Duration::from_millis(5),
            idle_cap: Duration::from_millis(20),
            failure_delay: Duration::from_millis(5),
            failure_delay_cap: Duration::from_millis(20),
            retry: fast_retry_schedule(),
            max_concurrency: 1,
            sandbox_spawn_execution_location: tidebreak_core::AgentRunExecutionLocation::InProcess,
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
            (messages[0].id, tidebreak_core::Role::User, "go"),
            (
                tidebreak_core::MessageId(steer_id.0),
                tidebreak_core::Role::User,
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
            (messages[0].id, tidebreak_core::Role::User, "go"),
            (
                tidebreak_core::MessageId(steer_id.0),
                tidebreak_core::Role::User,
                "queued direction",
            ),
            (messages[2].id, tidebreak_core::Role::Assistant, "hi"),
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

/// Park a turn on a client tool call without running the agent loop.
///
/// This and the other `park_*_for_route_test` helpers accept a turn and then
/// claim it straight from the store, which is a scan over the whole queue. The
/// app under test must therefore have no turn worker running yet, or the two
/// race for the same queued turn — see `test_app_without_turn_worker`.
async fn accept_and_claim_turn_for_route_test(
    store: &dyn Store,
    turn_id: TurnId,
    chat_id: ChatId,
    content: &str,
) -> (uuid::Uuid, chrono::DateTime<chrono::Utc>) {
    let accepted = match store
        .accept_turn(turn_id, chat_id, "fake", content)
        .await
        .unwrap()
    {
        tidebreak_core::AcceptTurnOutcome::Accepted(turn)
        | tidebreak_core::AcceptTurnOutcome::Existing(turn) => turn,
        outcome => panic!("route-test turn was not accepted: {outcome:?}"),
    };
    let turn_token = uuid::Uuid::new_v4();
    let claimed_at = accepted.available_at;
    let claimed = store
        .claim_turn(
            turn_token,
            claimed_at,
            claimed_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap()
        .turn
        .expect("the accepted route-test turn is due");
    assert_eq!(claimed.id, turn_id);
    (turn_token, claimed_at)
}

async fn park_client_wait_for_route_test(
    store: &dyn Store,
    chat_id: ChatId,
    progress: TurnCheckpointProgress,
) -> (TurnId, ClientToolCallRequest) {
    let turn_id = TurnId::new();
    let (turn_token, claimed_at) =
        accept_and_claim_turn_for_route_test(store, turn_id, chat_id, "native action").await;
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
            .park_turn_for_client_tool_call(turn_id, turn_token, 0, progress, claimed_at, &call,)
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
    let (turn_token, claimed_at) =
        accept_and_claim_turn_for_route_test(store, turn_id, chat_id, "ask a question").await;
    let call = ClientToolCallRequest {
        id: CallId::new(),
        chat_id,
        turn_id,
        provider_id: "provider-question".into(),
        name: tidebreak_core::ASK_USER_QUESTIONS_TOOL.into(),
        arguments: serde_json::json!({
            "questions": [{
                "id": "target",
                "header": "Target",
                "question": "Where should I deploy?",
                "options": [
                    {"id": "staging", "label": "Staging", "description": "Deploy for internal verification."},
                    {"id": "production", "label": "Production", "description": "Deploy to customers."}
                ],
                "question_type": "multi_select",
                "allow_free_form": true
            }, {
                "id": "note",
                "header": "Note",
                "question": "Anything else?",
                "question_type": "single_select",
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
            claimed_at,
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
        tidebreak_core::ResolveToolCallOutcome::Resolved
    );
    assert!(matches!(
        resolved.turn,
        Some(turn) if turn.status == TurnRunStatus::Resuming
    ));
}

#[tokio::test]
async fn user_question_api_is_renderer_safe_exact_and_not_native_claimable() {
    let (router, token, store, _dir) = test_app_without_turn_worker().await;
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
    assert_eq!(pending[0]["questions"][0]["question_type"], "multi_select");
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
            "answers": [{
                "question_id": "target",
                "selected_option_ids": ["unknown"]
            }]
        }),
    )
    .await;
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
    let answer = serde_json::json!({
        "answers": [{
            "question_id": "target",
            "selected_option_ids": ["staging", "production"],
            "custom_answer": "Start with a canary."
        }],
        "additional_user_context": "Keep the rollout reversible."
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
            "answers": [{
                "question_id": "target",
                "selected_option_ids": ["production"]
            }],
            "additional_user_context": "Keep the rollout reversible."
        }),
    )
    .await;
    assert_eq!(conflict.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn user_question_answer_announces_the_completion_live_once() {
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
    let (_turn_id, call) = park_user_questions_for_route_test(&*store, chat.id).await;

    // The answer resolves the call outside the agent loop, so this live frame
    // is the only thing settling the renderer's card before the turn ends.
    let mut live = state.events.subscribe(chat.id);
    let answer_uri = format!("/chats/{}/questions/{}/answer", chat.id, call.id);
    let answer = serde_json::json!({
        "answers": []
    });
    let answered = post_json(&router, &bearer, &answer_uri, answer.clone()).await;
    assert_eq!(answered.status(), StatusCode::OK);
    let announced = tokio::time::timeout(Duration::from_secs(1), live.recv())
        .await
        .expect("the answer must publish its completion live")
        .unwrap();
    let AgentEvent::ToolCallCompleted {
        call_id, output, ..
    } = &announced.event
    else {
        panic!("unexpected live event: {announced:?}");
    };
    assert_eq!(*call_id, call.id);
    assert!(!output.is_error);
    assert_eq!(
        store.list_events(chat.id, 0).await.unwrap().last(),
        Some(&announced)
    );

    // An exact retry recovers the committed answers without announcing twice.
    let retry = post_json(&router, &bearer, &answer_uri, answer).await;
    assert_eq!(retry.status(), StatusCode::OK);
    assert_eq!(
        json_body::<serde_json::Value>(retry).await["disposition"],
        "existing"
    );
    assert!(matches!(
        live.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
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
        created_at: chrono::Utc::now(),
        resolved_at: None,
    };
    let call = match store.accept_tool_call(&proposed_call).await.unwrap() {
        tidebreak_core::AcceptToolCallOutcome::Accepted(call)
        | tidebreak_core::AcceptToolCallOutcome::Existing(call) => call,
        tidebreak_core::AcceptToolCallOutcome::IdentityConflict => {
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
        name: tidebreak_core::REQUEST_FOLDER_ACCESS_TOOL.into(),
        arguments: serde_json::json!({
            "reason": "Read the project notes",
            "requested_capabilities": ["read_files"],
            "folder_hint": "documents",
        }),
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
        created_at: chrono::Utc::now(),
        resolved_at: None,
    };
    let request = match store.accept_tool_call(&request).await.unwrap() {
        tidebreak_core::AcceptToolCallOutcome::Accepted(call)
        | tidebreak_core::AcceptToolCallOutcome::Existing(call) => call,
        tidebreak_core::AcceptToolCallOutcome::IdentityConflict => panic!("fresh call conflicted"),
    };
    let unrelated = ToolCallRecord {
        id: CallId::new(),
        chat_id: chat.id,
        turn_id: TurnId::new(),
        provider_id: "other-provider-secret".into(),
        name: "connect_folder".into(),
        arguments: serde_json::json!({"host_path": "/Users/private"}),
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
        created_at: chrono::Utc::now(),
        resolved_at: None,
    };
    store.accept_tool_call(&unrelated).await.unwrap();
    let malformed = ToolCallRecord {
        id: CallId::new(),
        chat_id: chat.id,
        turn_id: TurnId::new(),
        provider_id: "malformed-provider-secret".into(),
        name: tidebreak_core::REQUEST_FOLDER_ACCESS_TOOL.into(),
        arguments: serde_json::json!({
            "reason": "Read /Users/private",
            "requested_capabilities": ["read_files"],
        }),
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
            name: tidebreak_core::REQUEST_FOLDER_ACCESS_TOOL.into(),
            arguments: serde_json::json!({
                "reason": reason,
                "requested_capabilities": ["read_files"],
            }),
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
    let (router, token, store, _dir) = test_app_without_turn_worker().await;
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
        name: tidebreak_core::REQUEST_FOLDER_ACCESS_TOOL.into(),
        arguments: serde_json::json!({
            "reason": "Read the project notes",
            "requested_capabilities": ["read_files"],
            "folder_hint": "documents",
        }),
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
        created_at: chrono::Utc::now(),
        resolved_at: None,
    };
    let folder_call = match store.accept_tool_call(&folder_call).await.unwrap() {
        tidebreak_core::AcceptToolCallOutcome::Accepted(call)
        | tidebreak_core::AcceptToolCallOutcome::Existing(call) => call,
        tidebreak_core::AcceptToolCallOutcome::IdentityConflict => {
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

/// The inbox is a read model with no state of its own: an item is listed while
/// its journal row is parked and gone once that row's own resolution route has
/// committed. Every kind is exercised in one pass because the value is the
/// aggregation, not any single branch of it.
#[tokio::test]
async fn the_inbox_lists_parked_work_until_its_own_route_resolves_it() {
    let (router, token, store, _dir) = test_app_without_turn_worker().await;
    let bearer = format!("Bearer {token}");

    let question_chat = make_chat(&router, &bearer).await;
    let (_question_turn, question_call) =
        park_user_questions_for_route_test(&*store, question_chat.id).await;

    let plan_chat = make_chat(&router, &bearer).await;
    let (_plan_turn, plan_call) = park_plan_for_route_test(&*store, plan_chat.id).await;

    let folder_chat = make_chat(&router, &bearer).await;
    let folder_access_call = park_folder_access_for_route_test(&*store, folder_chat.id).await;

    let approval_chat = make_chat(&router, &bearer).await;
    let approval_call = park_tool_approval_for_route_test(&*store, approval_chat.id).await;

    let listed = list_inbox(&router, &bearer).await;
    // The queue is conversations now (decision 48 step 3), each carrying the
    // calls parked behind it. Four chats, one parked call each.
    let kinds = listed
        .iter()
        .flat_map(|entry| entry["items"].as_array().expect("items is a list"))
        .map(|item| {
            (
                item["call_id"].as_str().unwrap().to_owned(),
                item["kind"].as_str().unwrap().to_owned(),
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(
        kinds,
        [
            (question_call.id.to_string(), "question".to_owned()),
            (plan_call.id.to_string(), "plan_review".to_owned()),
            (
                folder_access_call.id.to_string(),
                "folder_access".to_owned()
            ),
            (approval_call.to_string(), "tool_approval".to_owned()),
        ]
        .into_iter()
        .collect::<std::collections::BTreeMap<_, _>>(),
    );

    // Every entry states why it is listed, in the vocabulary code mode uses.
    for entry in &listed {
        assert_eq!(
            entry["attention"]["state"]["type"], "needs_you",
            "a parked call must read as needs-you: {entry}"
        );
    }

    // An entry carries its conversation and the parked call, which is what a
    // deep link needs to reopen the transcript where it stopped. The
    // conversation is tagged because chat and code ids are still separate
    // spaces; step 5 collapses that.
    let approval_entry = listed
        .iter()
        .find(|entry| entry["conversation"]["chat_id"] == approval_chat.id.to_string())
        .expect("the parked approval is listed");
    assert_eq!(approval_entry["conversation"]["surface"], "chat");
    assert_eq!(
        approval_entry["items"][0]["call_id"],
        approval_call.to_string()
    );
    assert_eq!(approval_entry["items"][0]["action"], "search");

    // Resolution goes through each kind's established route, unchanged.
    assert_eq!(
        answer_questions(&router, &bearer, question_chat.id, question_call.id).await,
        StatusCode::OK
    );
    assert_eq!(
        decide_plan_request(&router, &bearer, plan_chat.id, plan_call.id).await,
        StatusCode::OK
    );
    assert_eq!(
        decide_approval(&router, &bearer, approval_chat.id, approval_call, "approve").await,
        StatusCode::NO_CONTENT
    );
    resolve_parked_client_call(&*store, folder_chat.id, &folder_access_call).await;

    assert_eq!(
        list_inbox(&router, &bearer).await,
        Vec::<serde_json::Value>::new()
    );

    // First responder wins: answering an item a second time, differently,
    // meets the same conflict the in-chat card would have met.
    assert_eq!(
        decide_approval(&router, &bearer, approval_chat.id, approval_call, "reject").await,
        StatusCode::CONFLICT
    );
}

/// Decision 48 step 3: a chat carries the attention vocabulary code mode
/// introduced, so one supervising client watches one queue.
///
/// Derived, not stored — the assertions below are all about rows the inbox
/// already projects, and resolving an item through its own route is what
/// changes the answer. A stored copy would need a write at every one of those
/// points to say the same thing.
#[tokio::test]
async fn a_chat_carries_attention_derived_from_its_parked_work() {
    let (router, token, store, _dir) = test_app_without_turn_worker().await;
    let bearer = format!("Bearer {token}");
    let owner = tidebreak_core::OwnerId::local();

    let waiting = make_chat(&router, &bearer).await;
    let approval_call = park_tool_approval_for_route_test(&*store, waiting.id).await;
    let quiet = make_chat(&router, &bearer).await;

    let items = store.list_inbox_items_scoped(&owner).await.unwrap();
    let attention = store.chat_attention_scoped(&owner, &items).await.unwrap();

    let state = &attention.get(&waiting.id).expect("the waiting chat").state;
    assert!(
        matches!(state, tidebreak_core::AttentionState::NeedsYou { .. }),
        "a parked approval must read as needs-you, got {state:?}"
    );
    // The prompt names the kind and nothing else: an attention badge appears
    // in more places than the inbox does, and the inbox deliberately never
    // carries tool arguments or question text.
    if let tidebreak_core::AttentionState::NeedsYou { prompt, .. } = state {
        assert_eq!(prompt, "a tool call is waiting for approval");
    }

    // Absent means idle. Materializing a row for every settled conversation
    // would scale the read with history instead of with what is happening.
    assert!(
        !attention.contains_key(&quiet.id),
        "a chat with nothing parked must not appear"
    );

    assert_eq!(
        decide_approval(&router, &bearer, waiting.id, approval_call, "approve").await,
        StatusCode::NO_CONTENT
    );
    let items = store.list_inbox_items_scoped(&owner).await.unwrap();
    let attention = store.chat_attention_scoped(&owner, &items).await.unwrap();
    // Approving resumes the turn, so the chat stops waiting on the reader and
    // starts working. Asserting it goes quiet here would be asserting that
    // approving a call does nothing.
    assert_eq!(
        attention.get(&waiting.id).map(|value| &value.state),
        Some(&tidebreak_core::AttentionState::Working),
        "approving must hand the conversation back to the engine"
    );
}

async fn list_inbox(router: &Router, bearer: &str) -> Vec<serde_json::Value> {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/inbox")
                .header(header::AUTHORIZATION, bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    json_body(response).await
}

async fn park_plan_for_route_test(
    store: &dyn Store,
    chat_id: ChatId,
) -> (TurnId, ClientToolCallRequest) {
    let turn_id = TurnId::new();
    let (turn_token, claimed_at) =
        accept_and_claim_turn_for_route_test(store, turn_id, chat_id, "propose a plan").await;
    let call = ClientToolCallRequest {
        id: CallId::new(),
        chat_id,
        turn_id,
        provider_id: "provider-plan".into(),
        name: tidebreak_core::EXIT_PLAN_MODE_TOOL.into(),
        arguments: serde_json::json!({
            "title": "Migrate the importer",
            "plan": "## Steps\n\n1. Read the current importer.\n2. Write the replacement behind a flag.\n3. Cut over once the fixtures pass.",
        }),
    };
    store
        .park_turn_for_client_tool_call(
            turn_id,
            turn_token,
            0,
            test_client_checkpoint_progress(1),
            claimed_at,
            &call,
        )
        .await
        .unwrap()
        .unwrap();
    (turn_id, call)
}

async fn park_folder_access_for_route_test(
    store: &dyn Store,
    chat_id: ChatId,
) -> ClientToolCallRequest {
    let turn_id = TurnId::new();
    let (turn_token, claimed_at) =
        accept_and_claim_turn_for_route_test(store, turn_id, chat_id, "read the notes").await;
    let call = ClientToolCallRequest {
        id: CallId::new(),
        chat_id,
        turn_id,
        provider_id: "folder-provider".into(),
        name: tidebreak_core::REQUEST_FOLDER_ACCESS_TOOL.into(),
        arguments: serde_json::json!({
            "reason": "Read the project notes",
            "requested_capabilities": ["read_files"],
            "folder_hint": "documents",
        }),
    };
    store
        .park_turn_for_client_tool_call(
            turn_id,
            turn_token,
            0,
            test_client_checkpoint_progress(1),
            claimed_at,
            &call,
        )
        .await
        .unwrap()
        .unwrap();
    call
}

async fn park_tool_approval_for_route_test(store: &dyn Store, chat_id: ChatId) -> CallId {
    let turn_id = TurnId::new();
    store
        .accept_turn(turn_id, chat_id, "fake", "search the filings")
        .await
        .unwrap();
    let call_id = CallId::new();
    accept_server_tool_call_for_route_test(store, chat_id, turn_id, call_id).await;
    assert!(matches!(
        store
            .request_tool_call_approval(
                &tidebreak_core::ApprovalRequest {
                    call_id,
                    chat_id,
                    turn_id,
                    tool_name: "search".into(),
                    class: ApprovalClass::Sensitive,
                    kind: tidebreak_core::ToolApprovalKind::for_tool_name("search"),
                    preview: None,
                    auto_judge: false,
                },
                chrono::Utc::now(),
            )
            .await
            .unwrap(),
        tidebreak_core::RequestToolApprovalOutcome::Requested(_)
    ));
    call_id
}

async fn accept_server_tool_call_for_route_test(
    store: &dyn Store,
    chat_id: ChatId,
    turn_id: TurnId,
    call_id: CallId,
) {
    let record = ToolCallRecord {
        id: call_id,
        chat_id,
        turn_id,
        provider_id: format!("provider-{call_id}"),
        name: "search".into(),
        arguments: serde_json::json!({ "query": "quarterly filings" }),
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
        created_at: chrono::Utc::now(),
        resolved_at: None,
    };
    assert!(matches!(
        store.accept_tool_call(&record).await.unwrap(),
        tidebreak_core::AcceptToolCallOutcome::Accepted(_)
    ));
}

async fn answer_questions(
    router: &Router,
    bearer: &str,
    chat_id: ChatId,
    call_id: CallId,
) -> StatusCode {
    post_json(
        router,
        bearer,
        &format!("/chats/{chat_id}/questions/{call_id}/answer"),
        serde_json::json!({
            "answers": [
                {"question_id": "target", "selected_option_ids": ["staging"]},
                {"question_id": "note", "selected_option_ids": [], "custom_answer": "nothing else"}
            ]
        }),
    )
    .await
    .status()
}

async fn decide_plan_request(
    router: &Router,
    bearer: &str,
    chat_id: ChatId,
    call_id: CallId,
) -> StatusCode {
    post_json(
        router,
        bearer,
        &format!("/chats/{chat_id}/plans/{call_id}/decision"),
        serde_json::json!({"decision": "accept"}),
    )
    .await
    .status()
}

async fn decide_approval(
    router: &Router,
    bearer: &str,
    chat_id: ChatId,
    call_id: CallId,
    decision: &str,
) -> StatusCode {
    post_json(
        router,
        bearer,
        &format!("/chats/{chat_id}/approvals/{call_id}"),
        serde_json::json!({"decision": decision}),
    )
    .await
    .status()
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
        store.get_turn(resume_turn).await.unwrap().unwrap().status,
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
        tidebreak_core::RequestTurnCancellationOutcome::Requested(turn)
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
    // This helper claims from the whole turn queue. Park both cases before the
    // worker starts so it cannot race the helper for the exhausted turn.
    let exhausted_chat = make_chat(&router, &bearer).await;
    let exhausted_progress = test_client_checkpoint_progress(2);
    let (_, exhausted_call) =
        park_client_wait_for_route_test(&*store, exhausted_chat.id, exhausted_progress).await;

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

    resolve_parked_client_call(&*store, exhausted_chat.id, &exhausted_call).await;
    state.turn_job_wake.notify_one();
    let exhausted_events = wait_for_turn(&store, exhausted_chat.id).await;
    // The checkpoint spent the whole step budget, so the resuming segment has
    // zero steps left. It still owes the user a closing answer: the resume
    // runs the tool-free wrap-up call instead of failing (#1181), and the
    // wrap-up stays outside the budget in the durable accounting.
    assert!(matches!(
        exhausted_events.last().map(|event| &event.event),
        Some(AgentEvent::TurnCompleted { usage, .. })
            if *usage == Usage {
                input_tokens: exhausted_progress.usage.input_tokens + 2,
                output_tokens: exhausted_progress.usage.output_tokens + 1,
                cache_read_input_tokens: exhausted_progress.usage.cache_read_input_tokens,
                cache_creation_input_tokens: exhausted_progress.usage.cache_creation_input_tokens,
            }
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let exhausted_turn = store
        .list_turns(exhausted_chat.id)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(exhausted_turn.model_steps, 2);
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
    tools.register_client(
        ToolSpec {
            name: "connect_folder".into(),
            description: "Ask the desktop to connect a folder".into(),
            input_schema: serde_json::json!({"type": "object"}),
        },
        tidebreak_core::ApprovalClass::ReadOnly,
    );
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
    let parked = store.get_turn(turn_id).await.unwrap().unwrap();
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
                    tidebreak_core::ContentBlock::ToolUse { id, name, .. }
                        if id == "native_1" && name == "connect_folder"
                )
            })
        }));
        assert!(requests[1].messages.iter().any(|message| {
            message.content.iter().any(|block| {
                matches!(
                    block,
                    tidebreak_core::ContentBlock::ToolResult { tool_use_id, content, .. }
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
    exhausted_tools.register_client(
        ToolSpec {
            name: "connect_folder".into(),
            description: "Ask the desktop to connect a folder".into(),
            input_schema: serde_json::json!({"type": "object"}),
        },
        tidebreak_core::ApprovalClass::ReadOnly,
    );
    let exhausted_requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let exhausted_state = AppState::new(
        Config::desktop(exhausted_dir.path()),
        exhausted_store.clone(),
        Arc::new(FixedResolver(Arc::new(ClientThenFinishProvider {
            requests: exhausted_requests.clone(),
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
    let exhausted_router = app(exhausted_state.clone());
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
    // The client tool call landed on the last budgeted step. The turn still
    // parks — refusing here would throw the call away — and the resuming
    // zero-budget segment closes with the tool-free wrap-up call (#1181).
    let exhausted_pending = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let pending = exhausted_store
                .list_pending_client_tool_calls(exhausted_chat.id)
                .await
                .unwrap();
            if let Some(call) = pending.into_iter().next() {
                break call;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the last budgeted step still parks its client tool");
    let exhausted_parked = exhausted_store
        .get_turn(exhausted_turn_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(exhausted_parked.status, TurnRunStatus::WaitingForClient);
    assert_eq!(exhausted_parked.model_steps, 1);
    resolve_parked_client_call(
        &*exhausted_store,
        exhausted_chat.id,
        &ClientToolCallRequest {
            id: exhausted_pending.id,
            chat_id: exhausted_pending.chat_id,
            turn_id: exhausted_pending.turn_id,
            provider_id: exhausted_pending.provider_id.clone(),
            name: exhausted_pending.name.clone(),
            arguments: exhausted_pending.arguments.clone(),
        },
    )
    .await;
    exhausted_state.turn_job_wake.notify_one();
    let exhausted_events = wait_for_turn(&exhausted_store, exhausted_chat.id).await;
    assert!(matches!(
        exhausted_events.last().map(|event| &event.event),
        Some(AgentEvent::TurnCompleted { .. })
    ));
    {
        // The resumed segment had no steps left, so its one model call is the
        // wrap-up: tool calls forbidden, tool result in the transcript.
        let exhausted_requests = exhausted_requests.lock().unwrap();
        assert_eq!(exhausted_requests.len(), 2);
        assert_eq!(
            exhausted_requests[1].tool_choice,
            Some(tidebreak_core::ToolChoice::None)
        );
    }
    let exhausted_turn = exhausted_store
        .get_turn(exhausted_turn_id)
        .await
        .unwrap()
        .unwrap();
    // The wrap-up is outside the budget: only the tool step is counted.
    assert_eq!(exhausted_turn.model_steps, 1);
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
        tidebreak_core::ClaimClientToolCallOutcome::Claimed(_)
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
async fn client_execution_poll_terminalizes_an_expired_lease_and_resumes_the_turn() {
    let (router, token, store, _dir) = test_app_without_turn_worker().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let (turn_id, call) =
        park_client_wait_for_route_test(&*store, chat.id, test_client_checkpoint_progress(1)).await;
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
        tidebreak_core::ClaimClientToolCallOutcome::Claimed(_)
    ));
    tokio::time::sleep(Duration::from_millis(5)).await;

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
    assert!(json_body::<Vec<ToolCallRecord>>(pending).await.is_empty());

    let turn = store.get_turn(turn_id).await.unwrap().unwrap();
    assert_eq!(turn.status, TurnRunStatus::Resuming);
    let stored_call = store
        .list_tool_calls(chat.id)
        .await
        .unwrap()
        .into_iter()
        .find(|stored| stored.id == call.id)
        .unwrap();
    assert_eq!(stored_call.status, ToolCallStatus::Failed);
    assert_eq!(
        stored_call.error_code.as_deref(),
        Some("client_executor_lease_expired")
    );
    assert_eq!(
        stored_call.error_detail.as_deref(),
        Some("The client execution lease expired.")
    );

    let completion = store
        .list_events(chat.id, 0)
        .await
        .unwrap()
        .into_iter()
        .find(|event| {
            matches!(
                &event.event,
                AgentEvent::ToolCallCompleted { call_id, .. } if *call_id == call.id
            )
        })
        .expect("expired client call completion is journaled");
    let projected = serde_json::to_value(crate::event_projection::RendererSequencedEvent::from(
        &completion,
    ))
    .unwrap();
    assert_eq!(
        projected["event"]["failure"],
        serde_json::json!({
            "code": "executor_unavailable",
            "reason": "lease_expired",
        })
    );
    assert!(!projected
        .to_string()
        .contains("client_executor_lease_expired"));
}

#[tokio::test]
async fn cancellation_closes_an_expired_claimed_client_wait_without_executor_ack() {
    let (router, token, store, _dir) = test_app_without_turn_worker().await;
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;
    let (turn_id, call) =
        park_client_wait_for_route_test(&*store, chat.id, test_client_checkpoint_progress(1)).await;
    let claimed_at = chrono::Utc::now();
    store
        .claim_client_tool_call(
            call.id,
            chat.id,
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            claimed_at,
            claimed_at + chrono::Duration::milliseconds(2),
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(5)).await;

    assert_eq!(
        cancel_turn(&router, &bearer, chat.id, turn_id).await,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        store.get_turn(turn_id).await.unwrap().unwrap().status,
        TurnRunStatus::Cancelled
    );
    let stored_call = store
        .list_tool_calls(chat.id)
        .await
        .unwrap()
        .into_iter()
        .find(|stored| stored.id == call.id)
        .unwrap();
    assert_eq!(stored_call.status, ToolCallStatus::Cancelled);
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

/// The native-only surface requires a principal, not just the capability:
/// presenting only the client-executor credential names nobody and is
/// rejected before any handler runs.
#[tokio::test]
async fn client_executor_credential_alone_is_rejected() {
    let (router, _token, _store, _dir) = test_app().await;

    let response = router
        .oneshot(
            Request::builder()
                .uri("/sandbox-file-reads/pending")
                .header(
                    crate::auth::CLIENT_EXECUTOR_HEADER,
                    crate::state::TEST_CLIENT_EXECUTOR_TOKEN,
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

/// The auth middleware is the one place a principal is minted: the bearer
/// resolves to the local owner without the executor capability, the native
/// credential upgrades the capability bit on that principal, and the
/// capability alone cannot reach a handler. Probes the real middlewares in
/// the production layering (bearer outermost).
#[tokio::test]
async fn auth_middleware_resolves_principal_and_capability() {
    use crate::principal::{AuthContext, Principal};

    let (_app, token, state, _store, _dir) = test_app_with_state().await;
    let whoami = |auth: AuthContext| async move {
        format!(
            "{}|{}",
            matches!(auth.principal, Principal::LocalOwner),
            auth.client_executor
        )
    };
    let native = axum::Router::new()
        .route("/native", axum::routing::get(whoami))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::auth::require_client_executor_token,
        ));
    let probe = axum::Router::new()
        .route("/whoami", axum::routing::get(whoami))
        .merge(native)
        .route_layer(axum::middleware::from_fn_with_state(
            state,
            crate::auth::require_token,
        ));
    let bearer = format!("Bearer {token}");

    let request = |uri: &str, with_executor: bool| {
        let mut builder = Request::builder()
            .uri(uri)
            .header(header::AUTHORIZATION, &bearer);
        if with_executor {
            builder = builder.header(
                crate::auth::CLIENT_EXECUTOR_HEADER,
                crate::state::TEST_CLIENT_EXECUTOR_TOKEN,
            );
        }
        builder.body(Body::empty()).unwrap()
    };

    let response = probe
        .clone()
        .oneshot(request("/whoami", false))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 1024).await.unwrap();
    assert_eq!(&body[..], b"true|false", "bearer resolves the local owner");

    let response = probe
        .clone()
        .oneshot(request("/native", false))
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "a principal without the native credential lacks the capability"
    );

    let response = probe
        .clone()
        .oneshot(request("/native", true))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 1024).await.unwrap();
    assert_eq!(
        &body[..],
        b"true|true",
        "the native credential marks the capability on the same principal"
    );
}

/// The self-host boundary contract: the middleware resolves WHO from the
/// configured named tokens — a named token yields its user, an unknown token
/// answers 401, and the per-launch bearer (which names nobody on a shared
/// profile) is rejected too.
#[tokio::test]
async fn self_host_tokens_resolve_named_principals() {
    use crate::principal::{AuthContext, Principal};

    let (dir, store) = temp_db_store("self-host-auth.db").await;
    let store: Arc<dyn Store> = Arc::new(store);
    let tokens_file = dir.path().join("tokens");
    std::fs::write(
        &tokens_file,
        format!("# staff\nalice {ALICE_TOKEN} admin\n"),
    )
    .unwrap();
    let mut config = Config::desktop(dir.path());
    config.profile = tidebreak_core::Profile::SelfHost;
    config.auth_tokens_file = Some(tokens_file);
    let state = AppState::new(
        config,
        store,
        Arc::new(FixedResolver(Arc::new(FakeProvider))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    let launch_bearer = state.token.clone();

    let whoami = |auth: AuthContext| async move {
        match auth.principal {
            Principal::User { id, .. } => id.to_string(),
            other => format!("unexpected principal {other:?}"),
        }
    };
    let probe = axum::Router::new()
        .route("/whoami", axum::routing::get(whoami))
        .route_layer(axum::middleware::from_fn_with_state(
            state,
            crate::auth::require_token,
        ));
    let request = |bearer: &str| {
        Request::builder()
            .uri("/whoami")
            .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
            .body(Body::empty())
            .unwrap()
    };

    let response = probe.clone().oneshot(request(ALICE_TOKEN)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 1024).await.unwrap();
    assert_eq!(&body[..], b"alice", "the named token identifies its user");

    let response = probe.clone().oneshot(request(BOB_TOKEN)).await.unwrap();
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "a token the file does not name admits no one"
    );

    let response = probe
        .clone()
        .oneshot(request(&launch_bearer))
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "the per-launch bearer names nobody on a shared profile"
    );
}

/// The slice-4 boundary, exercised through real routes: handlers reach the
/// store only through the `ScopedStore` bound to the requesting principal, so
/// one user's root aggregates are invisible to another — reads, listings, and
/// mutations all answer as if the row does not exist. This is the route-level
/// face of the store partition test in `tidebreak-core`'s db suite.
#[tokio::test]
async fn routes_scope_root_aggregates_to_the_requesting_principal() {
    let (dir, store) = temp_db_store("self-host-scoping.db").await;
    let store: Arc<dyn Store> = Arc::new(store);
    let prompts_store = Arc::clone(&store);
    let tokens_file = dir.path().join("tokens");
    std::fs::write(
        &tokens_file,
        format!("alice {ALICE_TOKEN} admin\nbob {BOB_TOKEN}\n"),
    )
    .unwrap();
    let mut config = Config::desktop(dir.path());
    config.profile = tidebreak_core::Profile::SelfHost;
    config.auth_tokens_file = Some(tokens_file);
    let state = AppState::new(
        config,
        store,
        Arc::new(FixedResolver(Arc::new(FakeProvider))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    let router = app(state);
    let alice = format!("Bearer {ALICE_TOKEN}");
    let bob = format!("Bearer {BOB_TOKEN}");

    let chat = make_chat(&router, &alice).await;

    // Alice sees her chat; Bob's view holds no trace of it.
    let get = |bearer: &str, uri: String| {
        let bearer = bearer.to_owned();
        let router = router.clone();
        async move {
            router
                .oneshot(
                    Request::builder()
                        .uri(uri)
                        .header(header::AUTHORIZATION, bearer)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap()
        }
    };
    assert_eq!(
        get(&alice, format!("/chats/{}", chat.id)).await.status(),
        StatusCode::OK
    );
    assert_eq!(
        get(&bob, format!("/chats/{}", chat.id)).await.status(),
        StatusCode::NOT_FOUND,
        "another owner's chat must be indistinguishable from a missing one"
    );
    assert_eq!(
        get(&bob, format!("/chats/{}/messages", chat.id))
            .await
            .status(),
        StatusCode::NOT_FOUND,
        "the transcript behind the chat is equally invisible"
    );
    let listed: Vec<Chat> = json_body(get(&bob, "/chats".to_owned()).await).await;
    assert!(
        listed.is_empty(),
        "a listing never carries another owner's rows"
    );

    // The parked-work recovery routes hang off the same gate: a pending plan
    // approval or question set is only reachable through the chat that owns it.
    for path in ["plans/pending", "questions/pending", "task-plan"] {
        assert_eq!(
            get(&alice, format!("/chats/{}/{path}", chat.id))
                .await
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            get(&bob, format!("/chats/{}/{path}", chat.id))
                .await
                .status(),
            StatusCode::NOT_FOUND,
            "{path} must not answer for another owner's chat"
        );
    }

    // The cross-chat prompt summary is a root read, so it is scoped by owner
    // rather than by a chat id the caller names: Alice's parked question is
    // hers alone, and Bob's inbox does not even learn her chat exists.
    park_user_questions_for_route_test(&*prompts_store, chat.id).await;
    let alice_prompts: Vec<serde_json::Value> =
        json_body(get(&alice, "/chats/pending-prompts".to_owned()).await).await;
    assert_eq!(
        alice_prompts
            .iter()
            .filter(|summary| summary["chat_id"] == chat.id.to_string())
            .count(),
        1,
        "the owner still sees her own parked prompt"
    );
    let bob_prompts: Vec<serde_json::Value> =
        json_body(get(&bob, "/chats/pending-prompts".to_owned()).await).await;
    assert!(
        bob_prompts.is_empty(),
        "the prompt inbox never carries another owner's parked prompts"
    );

    // Mutations answer the same way: nothing to patch, nothing to delete.
    let patch = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/chats/{}", chat.id))
                .header(header::AUTHORIZATION, &bob)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"title": "taken over"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(patch.status(), StatusCode::NOT_FOUND);
    let delete = router
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/chats/{}", chat.id))
                .header(header::AUTHORIZATION, &bob)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(delete.status(), StatusCode::NOT_FOUND);

    // The chat survives the failed takeover, untouched.
    let survived = get(&alice, format!("/chats/{}", chat.id)).await;
    assert_eq!(survived.status(), StatusCode::OK);
    let survived: Chat = json_body(survived).await;
    assert_eq!(survived.title, None, "a cross-owner patch changed nothing");
}

/// The plan is durable and replaceable: a turn that calls `update_task_plan`
/// twice leaves exactly the second list behind the recovery route, with one
/// refresh hint per call and no steps in the journal itself.
#[tokio::test(flavor = "multi_thread")]
async fn a_turn_replaces_its_task_plan_and_journals_only_a_refresh_hint() {
    struct TwoPlansThenFinishProvider {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ModelProvider for TwoPlansThenFinishProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("two-plans-then-finish")
        }

        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            let plan = |id: &str, fragment: &str| {
                vec![
                    ProviderEvent::ToolCallStarted {
                        index: 0,
                        id: id.into(),
                        name: tidebreak_core::UPDATE_TASK_PLAN_TOOL.into(),
                    },
                    ProviderEvent::ToolCallArgsDelta {
                        index: 0,
                        fragment: fragment.into(),
                    },
                    ProviderEvent::Stop {
                        reason: StopReason::ToolUse,
                    },
                ]
            };
            let events = match call {
                0 => plan(
                    "plan_1",
                    r#"{"steps":[{"content":"Read the failing test","status":"in_progress"},{"content":"Fix the parser","status":"pending"}]}"#,
                ),
                1 => plan(
                    "plan_2",
                    r#"{"steps":[{"content":"Read the failing test","status":"completed"},{"content":"Fix the parser","status":"in_progress"}]}"#,
                ),
                _ => vec![
                    ProviderEvent::TextDelta {
                        text: "done".into(),
                    },
                    ProviderEvent::Stop {
                        reason: StopReason::EndTurn,
                    },
                ],
            };
            Ok(stream::iter(events).boxed())
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
    let mut tools = ToolRegistry::new();
    tools = tools.with(Box::new(crate::task_plan_tool::UpdateTaskPlanTool::new(
        store.clone(),
    )));
    let state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(Arc::new(TwoPlansThenFinishProvider {
            calls: Arc::new(AtomicUsize::new(0)),
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
        send_message_with_id(&router, &bearer, chat.id, turn_id, "do the long thing").await,
        StatusCode::ACCEPTED
    );

    let events = wait_for_turn(&store, chat.id).await;
    let hints: Vec<&SequencedEvent> = events
        .iter()
        .filter(|event| matches!(event.event, AgentEvent::TaskPlanUpdated { .. }))
        .collect();
    assert_eq!(hints.len(), 2, "one hint per call: {events:?}");
    for hint in hints {
        let AgentEvent::TaskPlanUpdated { turn_id: on, .. } = hint.event else {
            unreachable!("filtered above");
        };
        assert_eq!(on, turn_id);
        // The hint is bounded: nothing about the steps rides the journal.
        let payload = serde_json::to_value(&hint.event).unwrap();
        assert!(!payload.to_string().contains("Fix the parser"));
    }

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/chats/{}/task-plan", chat.id))
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let plan: Option<tidebreak_core::TaskPlan> = json_body(response).await;
    let plan = plan.expect("the turn recorded a plan");
    assert_eq!(plan.turn_id, turn_id);
    assert_eq!(
        plan.steps
            .iter()
            .map(|step| step.status)
            .collect::<Vec<_>>(),
        vec![
            tidebreak_core::TaskPlanStepStatus::Completed,
            tidebreak_core::TaskPlanStepStatus::InProgress,
        ],
        "the second call replaced the first list rather than merging into it"
    );
}

/// A headless run has no folder-consent surface, so `tidebreak -p` answers a
/// mid-turn `request_folder_access` itself with the contract's own `Declined`
/// result — the same answer a desktop prompt gives when the user closes it.
///
/// This is the shape the CLI's refusal has to keep: the turn resumes instead of
/// hanging on consent that can never arrive, the model is told access was
/// declined rather than that something failed, and nothing in the sequence can
/// end in a grant — declining is the only terminal a headless run can reach.
#[tokio::test(flavor = "multi_thread")]
async fn a_headless_folder_request_resumes_the_turn_with_the_declined_result() {
    struct FolderRequestThenFinishProvider {
        requests: Arc<std::sync::Mutex<Vec<ChatRequest>>>,
    }

    #[async_trait]
    impl ModelProvider for FolderRequestThenFinishProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("folder-request-then-finish")
        }

        async fn stream(&self, req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            let mut requests = self.requests.lock().unwrap();
            requests.push(req);
            let first = requests.len() == 1;
            drop(requests);
            let events = if first {
                vec![
                    ProviderEvent::ToolCallStarted {
                        index: 0,
                        id: "folder_1".into(),
                        name: tidebreak_core::REQUEST_FOLDER_ACCESS_TOOL.into(),
                    },
                    ProviderEvent::ToolCallArgsDelta {
                        index: 0,
                        fragment:
                            r#"{"reason":"Read the quarterly reports","requested_capabilities":["read_files"]}"#
                                .into(),
                    },
                    ProviderEvent::Stop {
                        reason: StopReason::ToolUse,
                    },
                ]
            } else {
                vec![
                    ProviderEvent::TextDelta {
                        text: "continuing without that folder".into(),
                    },
                    ProviderEvent::Stop {
                        reason: StopReason::EndTurn,
                    },
                ]
            };
            Ok(stream::iter(events).boxed())
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
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut tools = ToolRegistry::new();
    tools.register_validated_client(
        tidebreak_core::request_folder_access_tool_spec(),
        tidebreak_core::ApprovalClass::ReadOnly,
        tidebreak_core::validate_request_folder_access_arguments,
    );
    let state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(Arc::new(FolderRequestThenFinishProvider {
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
        send_message_with_id(&router, &bearer, chat.id, turn_id, "read my reports").await,
        StatusCode::ACCEPTED
    );

    let call = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(call) = store
                .list_pending_client_tool_calls(chat.id)
                .await
                .unwrap()
                .into_iter()
                .next()
            {
                break call;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the folder request should park the turn");
    assert_eq!(call.name, tidebreak_core::REQUEST_FOLDER_ACCESS_TOOL);
    assert_eq!(
        store.get_turn(turn_id).await.unwrap().unwrap().status,
        TurnRunStatus::WaitingForClient
    );

    // Exactly what `tidebreak -p` does on seeing the call: claim it, then
    // resolve it with the typed declined result.
    let lease_token = uuid::Uuid::new_v4();
    let claim = post_native_json(
        &router,
        &bearer,
        &format!("/chats/{}/client-executions/{}/claim", chat.id, call.id),
        serde_json::json!({
            "executor_id": uuid::Uuid::new_v4(),
            "lease_token": lease_token,
        }),
    )
    .await;
    assert_eq!(claim.status(), StatusCode::OK);
    let declined =
        serde_json::to_string(&tidebreak_core::RequestFolderAccessResult::Declined).unwrap();
    assert_eq!(declined, r#"{"status":"declined"}"#);
    let resolved = post_native_json(
        &router,
        &bearer,
        &format!("/chats/{}/client-executions/{}/resolve", chat.id, call.id),
        serde_json::json!({
            "lease_token": lease_token,
            "resolution": { "status": "completed", "result": declined },
        }),
    )
    .await;
    assert_eq!(resolved.status(), StatusCode::OK);
    state.turn_job_wake.notify_one();

    let events = wait_for_turn(&store, chat.id).await;
    assert!(
        matches!(
            events.last().map(|event| &event.event),
            Some(AgentEvent::TurnCompleted { .. })
        ),
        "the turn did not resume: {:?}",
        events.last()
    );
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[1].messages.iter().any(|message| {
            message.content.iter().any(|block| {
                matches!(
                    block,
                    tidebreak_core::ContentBlock::ToolResult { tool_use_id, content, .. }
                        if tool_use_id == "folder_1" && content == r#"{"status":"declined"}"#
                )
            })
        }),
        "the model was not told the folder request was declined"
    );
}

/// `POST /chats/{id}/queued/send-now` releases a paused queue: the promoter
/// runs the oldest message on its next sweep, and the row leaves the queue.
#[tokio::test]
async fn send_now_releases_a_paused_queue() {
    // The first turn parks in the provider, keeping the chat busy so the
    // follow-ups park as queued rows instead of running immediately.
    let gate = Arc::new(Notify::new());
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
        Arc::new(FixedResolver(Arc::new(GatedProvider {
            gate: gate.clone(),
        }))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        AgentConfig {
            model: "gated".into(),
            ..AgentConfig::default()
        },
    );
    let token = state.token.clone();
    let router = app(state.clone());
    spawn_turn_worker(&state);
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;

    assert_eq!(
        send_message_with_id(&router, &bearer, chat.id, TurnId::new(), "blocking turn").await,
        StatusCode::ACCEPTED
    );
    // Pause promotion. Through the store, not the route: a pause PUT that
    // lands while the gated turn is still being accepted would race the
    // promoter claiming that same turn and read as a leak below.
    store
        .set_setting(
            &format!("chats.{}.queue_paused", chat.id),
            &serde_json::json!(true),
        )
        .await
        .unwrap();
    for content in ["hold one", "hold two"] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/chats/{}/messages", chat.id))
                    .header(header::AUTHORIZATION, bearer.as_str())
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "turn_id": TurnId::new(),
                            "content": content,
                            "queue": true,
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
    }
    assert_eq!(
        store.list_queued_turns(chat.id).await.unwrap().len(),
        2,
        "the follow-ups did not park as queued rows"
    );

    // End the blocking turn. A paused promoter must leave both rows alone.
    gate.notify_one();
    wait_for_turn(&store, chat.id).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(
        store.list_queued_turns(chat.id).await.unwrap().len(),
        2,
        "a paused queue promoted a message"
    );

    let send_now = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/chats/{}/queued/send-now", chat.id))
                .header(header::AUTHORIZATION, bearer.as_str())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .status();
    assert_eq!(send_now, StatusCode::NO_CONTENT);

    // Tests build the router with `app()`, which never spawns the background
    // promoter task, so drive the sweep directly: the send-now route has
    // cleared the gate, promotion should now move one row per sweep while
    // the gate releases one parked turn at a time.
    for _ in 0..20 {
        crate::routes::promote_queued_turns(&state).await.unwrap();
        if store.list_queued_turns(chat.id).await.unwrap().is_empty() {
            return;
        }
        gate.notify_one();
        // Let the accepted turn reach its provider call before the next
        // sweep, or the promoter's `ChatBusy` just parks the row again.
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("send-now left rows in the queue");
}

/// The promoter is wake-driven: a message queued behind a running turn is
/// promoted promptly once that turn completes, without waiting out the
/// promoter's fallback tick.
#[tokio::test]
async fn queued_turn_promotes_on_completion_wake_without_the_fallback_tick() {
    // The first turn parks in the provider, keeping the chat busy so the
    // follow-up parks as a queued row instead of running immediately.
    let gate = Arc::new(Notify::new());
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
        Arc::new(FixedResolver(Arc::new(GatedProvider {
            gate: gate.clone(),
        }))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        AgentConfig {
            model: "gated".into(),
            ..AgentConfig::default()
        },
    );
    let token = state.token.clone();
    let router = app(state.clone());
    spawn_turn_worker(&state);
    // The real promoter loop, with a floor far beyond this test's timeout:
    // only a wake can promote the row in time, so a promotion below proves
    // the enqueue and turn-completion commits reach the promoter.
    tokio::spawn(crate::routes::run_queued_turn_promoter(
        state.clone(),
        Duration::from_secs(300),
    ));
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;

    assert_eq!(
        send_message_with_id(&router, &bearer, chat.id, TurnId::new(), "blocking turn").await,
        StatusCode::ACCEPTED
    );
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/chats/{}/messages", chat.id))
                .header(header::AUTHORIZATION, bearer.as_str())
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "turn_id": TurnId::new(),
                        "content": "queued follow-up",
                        "queue": true,
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert_eq!(
        store.list_queued_turns(chat.id).await.unwrap().len(),
        1,
        "the follow-up did not park as a queued row"
    );

    // Let the blocking turn finish. Its terminal commit must wake the
    // promoter, which drains the queued row long before the 300s floor.
    gate.notify_one();
    tokio::time::timeout(Duration::from_secs(10), async {
        while !store.list_queued_turns(chat.id).await.unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the queued message was not promoted off the completion wake");
}
