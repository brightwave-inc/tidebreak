use super::*;

/// Streams one text delta, then stalls forever — lets a test cancel mid-stream
/// at a known point (after the delta lands).
struct StallProvider;

#[async_trait]
impl ModelProvider for StallProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("stall")
    }
    async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        let head = stream::iter(vec![ProviderEvent::TextDelta {
            text: "partial".into(),
        }]);
        Ok(head.chain(stream::pending()).boxed())
    }
}

/// Gate that signals once a call is parked, then never resolves — so a test
/// can cancel a turn while it is genuinely waiting on approval.
struct SignalPendingGate {
    armed: std::sync::Mutex<Option<futures::channel::oneshot::Sender<()>>>,
}

impl ApprovalGate for SignalPendingGate {
    fn register(
        &self,
        _request: ApprovalRequest,
        _journal: Option<crate::approval::ApprovalJournalIdentity>,
    ) -> crate::approval::ApprovalRegistrationFuture<'_> {
        Box::pin(async move {
            if let Some(tx) = self.armed.lock().unwrap().take() {
                let _ = tx.send(());
            }
            crate::approval::ApprovalRegistration {
                decision: Box::pin(future::pending()) as crate::approval::ApprovalFuture,
                publication: crate::approval::ApprovalRequiredPublication::Ordinary,
            }
        })
    }
}

/// Trips cancel, then resolves Approve immediately — both arms of the
/// approval `select` are ready in the same poll. Without a cancel-preferring
/// check, `select` would take Approve and the Sensitive tool would run.
struct CancelThenApproveGate {
    cancel: CancelToken,
}

impl ApprovalGate for CancelThenApproveGate {
    fn register(
        &self,
        _request: ApprovalRequest,
        _journal: Option<crate::approval::ApprovalJournalIdentity>,
    ) -> crate::approval::ApprovalRegistrationFuture<'_> {
        Box::pin(async move {
            self.cancel.cancel();
            crate::approval::ApprovalRegistration {
                decision: Box::pin(async { ApprovalDecision::Approve })
                    as crate::approval::ApprovalFuture,
                publication: crate::approval::ApprovalRequiredPublication::Ordinary,
            }
        })
    }
}

struct ToolFutureDropMarker(Arc<AtomicBool>);

impl Drop for ToolFutureDropMarker {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

struct BlockingTool {
    entered: Arc<tokio::sync::Notify>,
    dropped: Arc<AtomicBool>,
}

#[async_trait]
impl Tool for BlockingTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "blocking".into(),
            description: "wait until the turn is cancelled".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::ReadOnly
    }

    async fn execute(&self, _ctx: &ToolCtx, _args: Value) -> Result<ToolOutput> {
        let _drop = ToolFutureDropMarker(self.dropped.clone());
        self.entered.notify_one();
        future::pending().await
    }
}

struct BlockingToolProvider;

#[async_trait]
impl ModelProvider for BlockingToolProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("blocking-tool")
    }

    async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        Ok(stream::iter(vec![
            ProviderEvent::ToolCallStarted {
                index: 0,
                id: "blocking_1".into(),
                name: "blocking".into(),
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

#[tokio::test]
async fn cancellation_drops_every_parallel_read_future() {
    struct ParallelBlockingRead {
        name: &'static str,
        entered: Arc<AtomicUsize>,
        both_entered: Arc<tokio::sync::Notify>,
        dropped: Arc<AtomicUsize>,
    }

    struct CountDrop(Arc<AtomicUsize>);

    impl Drop for CountDrop {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[async_trait]
    impl Tool for ParallelBlockingRead {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: self.name.into(),
                description: "waits for cancellation".into(),
                input_schema: serde_json::json!({"type": "object"}),
            }
        }

        fn approval_class(&self) -> ApprovalClass {
            ApprovalClass::ReadOnly
        }

        async fn execute(&self, _ctx: &ToolCtx, _args: Value) -> Result<ToolOutput> {
            let _drop = CountDrop(self.dropped.clone());
            if self.entered.fetch_add(1, Ordering::SeqCst) + 1 == 2 {
                self.both_entered.notify_one();
            }
            future::pending().await
        }
    }

    struct ParallelBlockingProvider;

    #[async_trait]
    impl ModelProvider for ParallelBlockingProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("parallel-blocking")
        }

        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            Ok(stream::iter(vec![
                ProviderEvent::ToolCallStarted {
                    index: 0,
                    id: "blocking_a".into(),
                    name: "blocking_a".into(),
                },
                ProviderEvent::ToolCallArgsDelta {
                    index: 0,
                    fragment: "{}".into(),
                },
                ProviderEvent::ToolCallStarted {
                    index: 1,
                    id: "blocking_b".into(),
                    name: "blocking_b".into(),
                },
                ProviderEvent::ToolCallArgsDelta {
                    index: 1,
                    fragment: "{}".into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::ToolUse,
                },
            ])
            .boxed())
        }
    }

    let (store, chat, _workspace) = cancel_test_chat().await;
    let cancel = CancelToken::new();
    let entered = Arc::new(AtomicUsize::new(0));
    let both_entered = Arc::new(tokio::sync::Notify::new());
    let dropped = Arc::new(AtomicUsize::new(0));
    let agent = Agent::new(
        Arc::new(ParallelBlockingProvider),
        Arc::new(
            ToolRegistry::new()
                .with(Box::new(ParallelBlockingRead {
                    name: "blocking_a",
                    entered: entered.clone(),
                    both_entered: both_entered.clone(),
                    dropped: dropped.clone(),
                }))
                .with(Box::new(ParallelBlockingRead {
                    name: "blocking_b",
                    entered: entered.clone(),
                    both_entered: both_entered.clone(),
                    dropped: dropped.clone(),
                })),
        ),
        store,
        AgentConfig {
            model: "parallel-blocking".into(),
            ..Default::default()
        },
    )
    .with_cancel(cancel.clone());

    let (tx, rx) = unbounded();
    let turn = tokio::spawn(async move { agent.run_turn(&chat, "go", &tx).await });
    let both_started =
        tokio::time::timeout(std::time::Duration::from_secs(1), both_entered.notified()).await;
    cancel.cancel();
    both_started.expect("both read-only calls should begin together");
    tokio::time::timeout(std::time::Duration::from_secs(1), turn)
        .await
        .expect("cancellation stops every parallel read")
        .unwrap()
        .unwrap();

    let events = rx.collect::<Vec<_>>().await;
    assert_eq!(entered.load(Ordering::SeqCst), 2);
    assert_eq!(dropped.load(Ordering::SeqCst), 2);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event,
                AgentEvent::ToolCallCompleted { output, .. }
                    if output.is_error && output.content == "turn cancelled during tool execution"
            ))
            .count(),
        2,
        "every admitted read receives a terminal cancellation result"
    );
}

#[tokio::test]
async fn cancel_before_the_turn_stops_before_any_model_call() {
    let (store, chat, _workspace) = cancel_test_chat().await;

    // A provider whose stream would panic the test if ever polled — proving
    // the loop-top check short-circuits before the first model call.
    let provider = FakeProvider {
        calls: AtomicUsize::new(0),
    };
    let cancel = CancelToken::new();
    cancel.cancel();
    let agent = Agent::new(
        Arc::new(provider),
        Arc::new(ToolRegistry::new()),
        store,
        AgentConfig {
            model: "fake".into(),
            ..Default::default()
        },
    )
    .with_cancel(cancel);

    let (tx, rx) = unbounded();
    agent.run_turn(&chat, "go", &tx).await.unwrap();
    drop(tx);
    let events: Vec<AgentEvent> = rx.collect().await;

    // Only the lifecycle bookends: started → cancelled, no model work between.
    assert!(matches!(
        events.first(),
        Some(AgentEvent::TurnStarted { .. })
    ));
    assert!(matches!(
        events.last(),
        Some(AgentEvent::TurnCancelled { .. })
    ));
    assert!(!events
        .iter()
        .any(|e| matches!(e, AgentEvent::TextDelta { .. })));
}

#[tokio::test]
async fn cancel_mid_stream_preempts_the_model_call() {
    let (store, chat, _workspace) = cancel_test_chat().await;

    let cancel = CancelToken::new();
    let agent = Agent::new(
        Arc::new(StallProvider),
        Arc::new(ToolRegistry::new()),
        store.clone(),
        AgentConfig {
            model: "stall".into(),
            ..Default::default()
        },
    )
    .with_cancel(cancel.clone());

    let (tx, mut rx) = unbounded();
    let chat_id = chat.id;
    let handle = tokio::spawn(async move {
        let _ = agent.run_turn(&chat, "go", &tx).await;
    });

    // Cancel the instant the first delta lands; the stream then stalls, so
    // only the cancel can end the turn.
    let mut cancelled = false;
    while let Some(event) = rx.next().await {
        match event {
            AgentEvent::TextDelta { text } if text == "partial" => cancel.cancel(),
            AgentEvent::TurnCancelled { .. } => cancelled = true,
            _ => {}
        }
    }
    handle.await.unwrap();

    assert!(cancelled, "a mid-stream cancel ends the turn as cancelled");
    // The prose the reader was already watching commits durably with the
    // cancellation, so the next model turn sees what was said (#1182).
    let messages = store.list_messages(chat_id).await.unwrap();
    let roles: Vec<Role> = messages.iter().map(|m| m.role).collect();
    assert_eq!(roles, vec![Role::User, Role::Assistant]);
    assert_eq!(messages[1].content, "partial");
}

/// The durable path's mid-stream cancel: the claimed outcome carries the
/// partial prose out for the worker to commit, and once committed the next
/// context load reads it annotated as user-stopped (#1182) while the
/// durable row keeps exactly what the user watched stream.
#[tokio::test]
async fn claimed_cancel_carries_partial_output_and_context_notes_the_stop() {
    let (store, chat, _workspace) = cancel_test_chat().await;

    let turn_id = TurnId::new();
    store
        .accept_turn(turn_id, chat.id, "stall", "go")
        .await
        .unwrap();
    let claimed_at = Utc::now();
    let lease = uuid::Uuid::new_v4();
    store
        .claim_turn_run(lease, claimed_at, claimed_at + chrono::Duration::minutes(1))
        .await
        .unwrap()
        .turn
        .expect("accepted turn is claimable");

    let cancel = CancelToken::new();
    let agent = Agent::new(
        Arc::new(StallProvider),
        Arc::new(ToolRegistry::new()),
        store.clone(),
        AgentConfig {
            model: "stall".into(),
            ..Default::default()
        },
    )
    .with_cancel(cancel.clone());

    let output_message_id = MessageId::new();
    let (tx, mut rx) = unbounded();
    let handle = tokio::spawn({
        let chat = chat.clone();
        async move {
            agent
                .run_claimed_turn(&chat, turn_id, output_message_id, 1, &tx)
                .await
        }
    });
    while let Some(emission) = rx.next().await {
        match emission {
            ClaimedAgentEvent::Pending {
                event: AgentEvent::TextDelta { .. },
                ..
            } => cancel.cancel(),
            ClaimedAgentEvent::Flush(ack) => {
                let _ = ack.send(());
            }
            _ => {}
        }
    }
    let outcome = handle.await.unwrap().unwrap();
    let AgentTurnOutcome::Cancelled {
        output,
        citations,
        usage,
        model_steps,
    } = outcome
    else {
        panic!("a mid-stream cancel ends the claimed turn as cancelled: {outcome:?}")
    };
    let output = output.expect("a prose-only cancel carries its partial output");
    assert_eq!(
        (output.id, output.content.as_str()),
        (output_message_id, "partial")
    );

    // Play the worker: durably request, then acknowledge with the output.
    store
        .request_turn_cancellation(turn_id, Utc::now())
        .await
        .unwrap()
        .expect("running cancellation is accepted");
    store
        .finish_turn_cancellation_and_append_event(
            turn_id,
            lease,
            Utc::now(),
            i32::try_from(model_steps).unwrap(),
            usage,
            Some(&output),
            &citations,
        )
        .await
        .unwrap()
        .expect("worker acknowledges cancellation with output");

    let stored = store.list_messages(chat.id).await.unwrap();
    assert_eq!(stored.last().map(|m| m.content.as_str()), Some("partial"));
    let transcript = agent_for_store(&store).load_transcript(chat.id, None).await;
    let assistant_text = transcript
        .unwrap()
        .messages
        .iter()
        .filter(|message| message.role == Role::Assistant)
        .flat_map(|message| message.content.iter())
        .find_map(|block| match block {
            ContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .expect("cancelled partial output reaches model context");
    assert_eq!(assistant_text, format!("partial{USER_INTERRUPTION_NOTE}"));
}

/// A throwaway agent over `store`, for exercising context loading.
fn agent_for_store(store: &Arc<dyn Store>) -> Agent {
    Agent::new(
        Arc::new(StallProvider),
        Arc::new(ToolRegistry::new()),
        store.clone(),
        AgentConfig {
            model: "stall".into(),
            ..Default::default()
        },
    )
}

struct ToolCallStallProvider;

#[async_trait]
impl ModelProvider for ToolCallStallProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("tool-stall")
    }
    async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        let head = stream::iter(vec![
            ProviderEvent::TextDelta {
                text: "partial".into(),
            },
            ProviderEvent::ToolCallStarted {
                index: 0,
                id: "call-0".into(),
                name: "echo".into(),
            },
        ]);
        Ok(head.chain(stream::pending()).boxed())
    }
}

/// A cancel that lands after `ToolCallStarted` was already journaled must
/// mark the call discarded, or replay and live clients hold a call that
/// never resolves. The marker is conditional — a cancel with only partial
/// prose must not send it, because replay clears visible assistant text on
/// the marker and cancellation deliberately retains that prose.
#[tokio::test]
async fn cancel_after_a_tool_call_starts_does_not_leave_it_dangling() {
    let (store, chat, _workspace) = cancel_test_chat().await;

    let cancel = CancelToken::new();
    let agent = Agent::new(
        Arc::new(ToolCallStallProvider),
        Arc::new(ToolRegistry::new()),
        store,
        AgentConfig {
            model: "tool-stall".into(),
            ..Default::default()
        },
    )
    .with_cancel(cancel.clone());

    let (tx, mut rx) = unbounded();
    let handle = tokio::spawn(async move {
        let _ = agent.run_turn(&chat, "go", &tx).await;
    });

    // Cancel the instant the started call is visible; the stream then
    // stalls, so only the cancel can end the turn.
    let mut events = Vec::new();
    while let Some(event) = rx.next().await {
        if matches!(event, AgentEvent::ToolCallStarted { .. }) {
            cancel.cancel();
        }
        events.push(event);
    }
    handle.await.unwrap();

    let started = events
        .iter()
        .position(|e| matches!(e, AgentEvent::ToolCallStarted { .. }));
    let interrupted = events
        .iter()
        .position(|e| matches!(e, AgentEvent::StreamInterrupted));
    let cancelled = events
        .iter()
        .position(|e| matches!(e, AgentEvent::TurnCancelled { .. }));
    assert!(
        matches!((started, interrupted, cancelled), (Some(a), Some(b), Some(c)) if a < b && b < c),
        "the started call is marked discarded before the turn terminalizes: {events:?}"
    );

    // The other half of the contract: with no started tool call the marker
    // stays unsent, so the partial prose the client already showed survives.
    let (store, chat, _workspace) = cancel_test_chat().await;
    let cancel = CancelToken::new();
    let agent = Agent::new(
        Arc::new(StallProvider),
        Arc::new(ToolRegistry::new()),
        store,
        AgentConfig {
            model: "stall".into(),
            ..Default::default()
        },
    )
    .with_cancel(cancel.clone());

    let (tx, mut rx) = unbounded();
    let handle = tokio::spawn(async move {
        let _ = agent.run_turn(&chat, "go", &tx).await;
    });

    let mut events = Vec::new();
    while let Some(event) = rx.next().await {
        if matches!(event, AgentEvent::TextDelta { .. }) {
            cancel.cancel();
        }
        events.push(event);
    }
    handle.await.unwrap();

    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::TurnCancelled { .. })),
        "a prose-only cancel still ends the turn as cancelled"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::StreamInterrupted)),
        "a cancel with no started tool call keeps the partial prose"
    );
}

#[tokio::test]
async fn cancel_drops_an_in_flight_server_tool_future() {
    let (store, chat, _workspace) = cancel_test_chat().await;
    let cancel = CancelToken::new();
    let entered = Arc::new(tokio::sync::Notify::new());
    let dropped = Arc::new(AtomicBool::new(false));
    let agent = Agent::new(
        Arc::new(BlockingToolProvider),
        Arc::new(ToolRegistry::new().with(Box::new(BlockingTool {
            entered: entered.clone(),
            dropped: dropped.clone(),
        }))),
        store,
        AgentConfig {
            model: "blocking-tool".into(),
            ..Default::default()
        },
    )
    .with_cancel(cancel.clone());

    let (tx, rx) = unbounded();
    let handle = tokio::spawn(async move {
        agent.run_turn(&chat, "go", &tx).await.unwrap();
    });

    entered.notified().await;
    cancel.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(1), handle)
        .await
        .expect("cancellation should stop an in-flight tool promptly")
        .unwrap();
    let events = rx.collect::<Vec<_>>().await;

    assert!(
        dropped.load(Ordering::SeqCst),
        "cancellation must drop the tool future so its HTTP request can abort"
    );
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ToolCallCompleted { output, .. }
            if output.is_error && output.content == "turn cancelled during tool execution"
    )));
    assert!(matches!(
        events.last(),
        Some(AgentEvent::TurnCancelled { .. })
    ));
}

#[tokio::test]
async fn cancel_unblocks_a_turn_parked_on_approval() {
    let (store, chat, _workspace) = cancel_test_chat().await;

    let (armed_tx, armed_rx) = futures::channel::oneshot::channel();
    let gate = Arc::new(SignalPendingGate {
        armed: std::sync::Mutex::new(Some(armed_tx)),
    });
    let ran = Arc::new(AtomicUsize::new(0));
    let cancel = CancelToken::new();
    let agent = Agent::new(
        Arc::new(BoomProvider {
            calls: AtomicUsize::new(0),
        }),
        Arc::new(ToolRegistry::new().with(Box::new(BoomTool { ran: ran.clone() }))),
        store,
        AgentConfig {
            model: "fake".into(),
            ..Default::default()
        },
    )
    .with_approvals(gate)
    .with_cancel(cancel.clone());

    let (tx, rx) = unbounded();
    let handle = tokio::spawn(async move {
        let _ = agent.run_turn(&chat, "go", &tx).await;
    });

    // Wait until the Sensitive call is genuinely parked, then cancel.
    armed_rx.await.unwrap();
    cancel.cancel();
    handle.await.unwrap();
    let events: Vec<AgentEvent> = rx.collect().await;

    assert_eq!(ran.load(Ordering::SeqCst), 0, "the parked tool never runs");
    assert!(events
        .iter()
        .any(|e| matches!(e, AgentEvent::ApprovalRequired { .. })));
    assert!(matches!(
        events.last(),
        Some(AgentEvent::TurnCancelled { .. })
    ));
}

#[tokio::test]
async fn cancel_wins_when_approval_and_cancel_are_both_ready() {
    let (store, chat, _workspace) = cancel_test_chat().await;

    let ran = Arc::new(AtomicUsize::new(0));
    let cancel = CancelToken::new();
    let agent = Agent::new(
        Arc::new(BoomProvider {
            calls: AtomicUsize::new(0),
        }),
        Arc::new(ToolRegistry::new().with(Box::new(BoomTool { ran: ran.clone() }))),
        store,
        AgentConfig {
            model: "fake".into(),
            ..Default::default()
        },
    )
    .with_approvals(Arc::new(CancelThenApproveGate {
        cancel: cancel.clone(),
    }))
    .with_cancel(cancel);

    let (tx, rx) = unbounded();
    agent.run_turn(&chat, "go", &tx).await.unwrap();
    drop(tx);
    let events: Vec<AgentEvent> = rx.collect().await;

    assert_eq!(
        ran.load(Ordering::SeqCst),
        0,
        "cancel must preempt an approve that is ready in the same poll"
    );
    assert!(!events
        .iter()
        .any(|e| matches!(e, AgentEvent::ApprovalDecided { approved: true, .. })));
    assert!(matches!(
        events.last(),
        Some(AgentEvent::TurnCancelled { .. })
    ));
}

#[tokio::test]
async fn interrupt_steer_preempts_mid_stream_and_continues() {
    let (store, chat, _workspace) = cancel_test_chat().await;

    // First call stalls after "partial"; after steer, second call finishes.
    struct StallThenFinish {
        calls: AtomicUsize,
    }
    #[async_trait]
    impl ModelProvider for StallThenFinish {
        fn id(&self) -> ProviderId {
            ProviderId::new("stall-then-finish")
        }
        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
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

    let steer = SteerInbox::new();
    let agent = Agent::new(
        Arc::new(StallThenFinish {
            calls: AtomicUsize::new(0),
        }),
        Arc::new(ToolRegistry::new()),
        store.clone(),
        AgentConfig {
            model: "stall".into(),
            ..Default::default()
        },
    )
    .with_steer(steer.clone());

    let chat_id = chat.id;
    let (tx, mut rx) = unbounded();
    let handle = tokio::spawn(async move {
        let _ = agent.run_turn(&chat, "go", &tx).await;
    });

    let mut steered = false;
    let mut interrupted = false;
    let mut completed = false;
    while let Some(event) = rx.next().await {
        match event {
            AgentEvent::TextDelta { text } if text == "partial" => {
                steer.push("please change course", true);
            }
            AgentEvent::StreamInterrupted => {
                interrupted = true;
            }
            AgentEvent::UserSteered { content, .. } => {
                assert_eq!(content, "please change course");
                steered = true;
            }
            AgentEvent::TurnCompleted { .. } => completed = true,
            AgentEvent::TurnCancelled { .. } => {
                panic!("steer must continue the turn, not cancel it")
            }
            _ => {}
        }
    }
    handle.await.unwrap();

    assert!(
        interrupted,
        "interrupt steer marks the partial provider stream as abandoned"
    );
    assert!(steered, "steer event emitted");
    assert!(completed, "turn completes after steer");
    let roles: Vec<_> = store
        .list_messages(chat_id)
        .await
        .unwrap()
        .iter()
        .map(|m| (m.role, m.content.clone()))
        .collect();
    // Initial user + steered user + final assistant (partial discarded).
    assert!(roles.iter().any(|(r, c)| *r == Role::User && c == "go"));
    assert!(roles
        .iter()
        .any(|(r, c)| *r == Role::User && c == "please change course"));
    assert!(roles
        .iter()
        .any(|(r, c)| *r == Role::Assistant && c == "after steer"));
    assert!(!roles.iter().any(|(_, c)| c == "partial"));
}

#[tokio::test]
async fn boundary_steer_persists_distinct_legacy_assistant_candidates() {
    let (store, chat, _workspace) = cancel_test_chat().await;

    struct BoundaryThenFinish {
        calls: AtomicUsize,
        release: Mutex<Option<futures::channel::oneshot::Receiver<()>>>,
    }
    #[async_trait]
    impl ModelProvider for BoundaryThenFinish {
        fn id(&self) -> ProviderId {
            ProviderId::new("boundary-then-finish")
        }

        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                let release = self.release.lock().unwrap().take().unwrap();
                return Ok(stream::iter(vec![ProviderEvent::TextDelta {
                    text: "first candidate".into(),
                }])
                .chain(stream::once(async move {
                    let _ = release.await;
                    ProviderEvent::Stop {
                        reason: StopReason::EndTurn,
                    }
                }))
                .boxed());
            }
            Ok(stream::iter(vec![
                ProviderEvent::TextDelta {
                    text: "final candidate".into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ])
            .boxed())
        }
    }

    let (release_tx, release_rx) = futures::channel::oneshot::channel();
    let steer = SteerInbox::new();
    let agent = Agent::new(
        Arc::new(BoundaryThenFinish {
            calls: AtomicUsize::new(0),
            release: Mutex::new(Some(release_rx)),
        }),
        Arc::new(ToolRegistry::new()),
        store.clone(),
        AgentConfig {
            model: "fake".into(),
            ..Default::default()
        },
    )
    .with_steer(steer.clone());

    let chat_id = chat.id;
    let (tx, mut rx) = unbounded();
    let run = tokio::spawn(async move { agent.run_turn(&chat, "go", &tx).await });
    while let Some(event) = rx.next().await {
        if matches!(
            event,
            AgentEvent::TextDelta { ref text } if text == "first candidate"
        ) {
            assert!(steer.push("revise that", false));
            let _ = release_tx.send(());
            break;
        }
    }
    run.await.unwrap().unwrap();

    let messages = store.list_messages(chat_id).await.unwrap();
    assert_eq!(messages.len(), 4);
    assert_eq!(messages[0].content, "go");
    assert_eq!(messages[1].content, "first candidate");
    assert_eq!(messages[2].content, "revise that");
    assert_eq!(messages[3].content, "final candidate");
    assert_ne!(messages[1].id, messages[3].id);
}

#[tokio::test]
async fn cancel_wins_over_steer_when_both_ready() {
    let (store, chat, _workspace) = cancel_test_chat().await;

    let cancel = CancelToken::new();
    let steer = SteerInbox::new();
    // Trip both before the turn starts racing the stream.
    cancel.cancel();
    steer.push("ignored", true);

    let agent = Agent::new(
        Arc::new(StallProvider),
        Arc::new(ToolRegistry::new()),
        store,
        AgentConfig {
            model: "stall".into(),
            ..Default::default()
        },
    )
    .with_cancel(cancel)
    .with_steer(steer);

    let (tx, rx) = unbounded();
    agent.run_turn(&chat, "go", &tx).await.unwrap();
    drop(tx);
    let events: Vec<AgentEvent> = rx.collect().await;

    assert!(matches!(
        events.last(),
        Some(AgentEvent::TurnCancelled { .. })
    ));
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::UserSteered { .. })),
        "cancel must win; steer is not applied"
    );
}
