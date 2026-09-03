use super::*;

struct SemanticCheckpointProvider {
    requests: Arc<Mutex<Vec<ChatRequest>>>,
    summary_calls: Arc<AtomicUsize>,
    foreground_calls: Arc<AtomicUsize>,
    malformed_summary: bool,
    tool_first: bool,
    /// Answer the compaction call with a tool call instead of a summary — the
    /// request advertises the conversation's tools, so a model may take one.
    checkpoint_calls_tool: bool,
}

#[async_trait]
impl ModelProvider for SemanticCheckpointProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("semantic-checkpoint")
    }

    async fn stream(&self, request: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        // The compaction call is the conversation's own request with one
        // instruction appended, so its trailing message is what tells it apart —
        // prefix rather than equality, because a compaction the user asked for
        // carries their focus line after the standing instructions.
        let maintenance = is_checkpoint_request(&request);
        self.requests.lock().unwrap().push(request);
        if maintenance {
            self.summary_calls.fetch_add(1, Ordering::SeqCst);
            if self.checkpoint_calls_tool {
                return Ok(stream::iter(vec![
                    ProviderEvent::ToolCallStarted {
                        index: 0,
                        id: "checkpoint_maintenance_tool".into(),
                        name: "checkpoint_noop".into(),
                    },
                    ProviderEvent::ToolCallArgsDelta {
                        index: 0,
                        fragment: "{}".into(),
                    },
                    ProviderEvent::Stop {
                        reason: StopReason::ToolUse,
                    },
                ])
                .boxed());
            }
            let text = if self.malformed_summary {
                "not a structured checkpoint"
            } else {
                r#"{"version":2,"original_requests":[],"confirmed_decisions":["Use the durable SQLite path."],"unresolved_questions":["Confirm the rollout date."],"task_state":["Migration implementation is in progress."],"source_identities":["source:decision-doc"],"output_identities":["output:migration-plan"],"conclusions":["The local path preserves exact retries."]}"#
            };
            return Ok(stream::iter(vec![
                ProviderEvent::TextDelta { text: text.into() },
                ProviderEvent::Usage(Usage {
                    input_tokens: 100,
                    output_tokens: 20,
                    cache_read_input_tokens: 10,
                    cache_creation_input_tokens: 5,
                }),
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ])
            .boxed());
        }

        let call = self.foreground_calls.fetch_add(1, Ordering::SeqCst);
        if self.tool_first && call == 0 {
            return Ok(stream::iter(vec![
                ProviderEvent::Usage(Usage {
                    input_tokens: 7,
                    output_tokens: 3,
                    ..Usage::default()
                }),
                ProviderEvent::ToolCallStarted {
                    index: 0,
                    id: "checkpoint_tool_1".into(),
                    name: "checkpoint_noop".into(),
                },
                ProviderEvent::ToolCallArgsDelta {
                    index: 0,
                    fragment: "{}".into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::ToolUse,
                },
            ])
            .boxed());
        }
        Ok(stream::iter(vec![
            ProviderEvent::TextDelta {
                text: "done".into(),
            },
            ProviderEvent::Usage(Usage {
                input_tokens: 5,
                output_tokens: 2,
                ..Usage::default()
            }),
            ProviderEvent::Stop {
                reason: StopReason::EndTurn,
            },
        ])
        .boxed())
    }
}

/// Whether this is the compaction call: the conversation's request plus one
/// trailing instruction message.
fn is_checkpoint_request(request: &ChatRequest) -> bool {
    request.messages.last().is_some_and(|message| {
        message.content.iter().any(|block| {
            matches!(block, ContentBlock::Text { text } if text.starts_with(CONTEXT_CHECKPOINT_INSTRUCTION))
        })
    })
}

/// Production defaults floor the trigger at 50k tokens and protect the five
/// newest rows, which no small-window test can reach. These keep the same
/// percentage hysteresis while letting a few-thousand-token transcript cross
/// the threshold and still leave a compactable prefix.
fn test_compaction_policy() -> CompactionPolicy {
    CompactionPolicy {
        threshold_fraction: 0.75,
        target_fraction: 0.25,
        min_threshold_tokens: 0,
        protect_recent_messages: 2,
    }
}

async fn append_semantic_checkpoint_history(
    store: &Arc<dyn Store>,
    chat_id: SessionId,
) -> Vec<Message> {
    let messages = vec![
        Message {
            id: MessageId::new(),
            chat_id,
            turn_id: TurnId::new(),
            role: Role::User,
            reasoning: Default::default(),
            content: format!(
                "OLD PREFIX: choose the durable SQLite path. {}",
                "historical detail ".repeat(1_200)
            ),
            llm_content: None,
            created_at: Utc::now(),
        },
        Message {
            id: MessageId::new(),
            chat_id,
            turn_id: TurnId::new(),
            role: Role::Assistant,
            reasoning: Default::default(),
            content: "OLD ASSISTANT: SQLite is confirmed; source:decision-doc.".into(),
            llm_content: None,
            created_at: Utc::now(),
        },
        Message {
            id: MessageId::new(),
            chat_id,
            turn_id: TurnId::new(),
            role: Role::User,
            reasoning: Default::default(),
            content: "RECENT USER: keep this exchange raw.".into(),
            llm_content: None,
            created_at: Utc::now(),
        },
        Message {
            id: MessageId::new(),
            chat_id,
            turn_id: TurnId::new(),
            role: Role::Assistant,
            reasoning: Default::default(),
            content: "RECENT ASSISTANT: this is the newest completed exchange.".into(),
            llm_content: None,
            created_at: Utc::now(),
        },
    ];
    for message in &messages {
        store.append_message(message).await.unwrap();
    }
    messages
}

#[tokio::test]
async fn creates_projects_and_deduplicates_a_structured_semantic_checkpoint() {
    let (store, chat, _workspace) = cancel_test_chat().await;
    let history = append_semantic_checkpoint_history(&store, chat.id).await;
    let requests = Arc::new(Mutex::new(Vec::new()));
    let summary_calls = Arc::new(AtomicUsize::new(0));
    let foreground_calls = Arc::new(AtomicUsize::new(0));
    let vendor_search = VendorWebSearch { max_uses: 3 };
    let provider = Arc::new(SemanticCheckpointProvider {
        requests: requests.clone(),
        summary_calls: summary_calls.clone(),
        foreground_calls: foreground_calls.clone(),
        malformed_summary: false,
        tool_first: true,
        checkpoint_calls_tool: false,
    });
    let agent = Agent::new(
        provider,
        Arc::new(ToolRegistry::new().with(Box::new(CheckpointNoopTool))),
        store.clone(),
        AgentConfig {
            model: "small-context-model".into(),
            context_window: 3_000,
            compaction: test_compaction_policy(),
            web_search: TurnWebSearch::Vendor(vendor_search),
            ..Default::default()
        },
    );

    let (tx, rx) = unbounded();
    agent
        .run_turn(&chat, "CURRENT USER: continue the migration.", &tx)
        .await
        .unwrap();
    drop(tx);
    let events = rx.collect::<Vec<_>>().await;

    assert_eq!(summary_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        foreground_calls.load(Ordering::SeqCst),
        2,
        "a second foreground tool step must not recursively summarize"
    );
    let checkpoint = store
        .get_context_checkpoint(chat.id)
        .await
        .unwrap()
        .expect("the reduced prefix is checkpointed");
    assert_eq!(
        checkpoint.source_message_id, history[0].id,
        "compaction cuts back to the oldest row the raw-history target cannot keep"
    );
    assert_eq!(
        checkpoint.format_version,
        crate::CONTEXT_CHECKPOINT_FORMAT_V2
    );
    assert_eq!(
        checkpoint.usage,
        Usage {
            input_tokens: 100,
            output_tokens: 20,
            cache_read_input_tokens: 10,
            cache_creation_input_tokens: 5,
        },
        "maintenance usage is durable on the checkpoint"
    );
    let payload: ContextCheckpointPayloadV2 = serde_json::from_str(&checkpoint.content).unwrap();
    assert_eq!(
        payload.confirmed_decisions,
        ["Use the durable SQLite path."]
    );
    // The host, not the summarizer, owns `original_requests`: the founding ask
    // in the compacted prefix has to survive even though the model returned an
    // empty list.
    assert!(
        payload
            .original_requests
            .iter()
            .any(|request| request.contains("OLD PREFIX")),
        "the compacted user ask carries forward: {:?}",
        payload.original_requests
    );

    let requests = requests.lock().unwrap();
    let checkpoint_request = requests
        .iter()
        .find(|request| is_checkpoint_request(request))
        .expect("one checkpoint request");
    assert_eq!(
        checkpoint_request.model, "small-context-model",
        "the checkpoint call runs on the conversation's model, on its route"
    );
    // Both are expressed by editing the tool array on the Messages API, so
    // either would throw away the cache this call exists to read.
    assert!(checkpoint_request.response_format.is_none());
    assert!(checkpoint_request.tool_choice.is_none());
    assert_eq!(
        checkpoint_request.vendor_web_search, None,
        "maintenance cannot receive a second provider search budget for the turn"
    );
    let checkpoint_debug = format!("{:?}", checkpoint_request.messages);
    assert!(checkpoint_debug.contains("OLD PREFIX"));

    let foreground = requests
        .iter()
        .filter(|request| !is_checkpoint_request(request))
        .collect::<Vec<_>>();
    assert!(foreground
        .iter()
        .all(|request| request.vendor_web_search == Some(vendor_search)));
    assert!(foreground.iter().all(|request| request.messages.iter().any(
            |message| message.content.iter().any(
                |block| matches!(block, ContentBlock::Text { text } if text.contains(CHECKPOINT_CONTEXT_PREFIX)),
            ),
        )));
    assert!(!context::has_orphaned_tool_blocks(
        &foreground.last().unwrap().messages
    ));
    assert!(foreground.last().unwrap().messages.iter().any(|message| {
            message.content.iter().any(
                |block| matches!(block, ContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == "checkpoint_tool_1"),
            )
        }));

    let turn_usage = events.iter().find_map(|event| match event {
        AgentEvent::TurnCompleted { usage, .. } => Some(*usage),
        _ => None,
    });
    assert_eq!(
        turn_usage,
        Some(Usage {
            input_tokens: 12,
            output_tokens: 5,
            ..Usage::default()
        }),
        "checkpoint usage is not charged to the user-visible turn"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::ContextTruncated { .. })),
        "standing a checkpoint in for its own prefix is compaction, which the \
         compaction events report — not deterministic truncation"
    );
    let compaction: Vec<&AgentEvent> = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                AgentEvent::CompactionStarted | AgentEvent::CompactionFinished { .. }
            )
        })
        .collect();
    assert!(
        matches!(
            compaction.as_slice(),
            [
                AgentEvent::CompactionStarted,
                AgentEvent::CompactionFinished { compacted: true }
            ]
        ),
        "one compaction runs, and it reports success: {compaction:?}"
    );
}

#[tokio::test]
async fn compacting_on_request_ignores_the_threshold_and_steers_the_summary() {
    let (store, chat, _workspace) = cancel_test_chat().await;
    append_semantic_checkpoint_history(&store, chat.id).await;
    let requests = Arc::new(Mutex::new(Vec::new()));
    let summary_calls = Arc::new(AtomicUsize::new(0));
    let provider = Arc::new(SemanticCheckpointProvider {
        requests: requests.clone(),
        summary_calls: summary_calls.clone(),
        foreground_calls: Arc::new(AtomicUsize::new(0)),
        malformed_summary: false,
        tool_first: false,
        checkpoint_calls_tool: false,
    });
    // A window this chat comes nowhere near filling: the automatic pass would
    // decline, so a checkpoint here is the request itself acting as the trigger.
    let agent = Agent::new(
        provider,
        Arc::new(ToolRegistry::new()),
        store.clone(),
        AgentConfig {
            model: "large-context-model".into(),
            context_window: 1_000_000,
            compaction: CompactionPolicy {
                threshold_fraction: 0.75,
                target_fraction: 0.0001,
                min_threshold_tokens: 0,
                protect_recent_messages: 2,
            },
            ..Default::default()
        },
    );

    let (tx, rx) = unbounded();
    let created = agent
        .compact_now(&chat, Some("the rollout date"), &tx)
        .await
        .unwrap();
    drop(tx);
    let events = rx.collect::<Vec<_>>().await;

    assert!(
        created.is_some(),
        "the requested compaction wrote a checkpoint"
    );
    assert_eq!(summary_calls.load(Ordering::SeqCst), 1);
    assert!(
        matches!(
            events.as_slice(),
            [
                AgentEvent::CompactionStarted,
                AgentEvent::CompactionFinished { compacted: true }
            ]
        ),
        "the caller sees the same pair a turn's compaction reports: {events:?}"
    );
    let requests = requests.lock().unwrap();
    assert!(
        is_checkpoint_request(&requests[0]),
        "the standing instructions survive the focus line"
    );
    let ContentBlock::Text { text } = &requests[0]
        .messages
        .last()
        .expect("the call carries its instructions")
        .content[0]
    else {
        panic!("the instruction is one text block");
    };
    assert!(text.contains("the rollout date"));
}

/// A checkpoint the host cannot parse must cost the turn nothing — and the
/// declined attempt is also where the cache contract is legible: the summary
/// wrote no checkpoint, so the step that follows sends the exact prefix the
/// checkpoint call was built from.
#[tokio::test]
async fn malformed_checkpoint_summary_fails_open_to_deterministic_reduction() {
    let (store, chat, _workspace) = cancel_test_chat().await;
    append_semantic_checkpoint_history(&store, chat.id).await;
    let requests = Arc::new(Mutex::new(Vec::new()));
    let summary_calls = Arc::new(AtomicUsize::new(0));
    let foreground_calls = Arc::new(AtomicUsize::new(0));
    let agent = Agent::new(
        Arc::new(SemanticCheckpointProvider {
            requests: requests.clone(),
            summary_calls: summary_calls.clone(),
            foreground_calls: foreground_calls.clone(),
            malformed_summary: true,
            tool_first: true,
            checkpoint_calls_tool: false,
        }),
        Arc::new(ToolRegistry::new().with(Box::new(CheckpointNoopTool))),
        store.clone(),
        AgentConfig {
            model: "small-context-model".into(),
            context_window: 3_000,
            compaction: test_compaction_policy(),
            ..Default::default()
        },
    );

    let (tx, rx) = unbounded();
    agent.run_turn(&chat, "continue", &tx).await.unwrap();
    drop(tx);
    let events = rx.collect::<Vec<_>>().await;
    assert_eq!(summary_calls.load(Ordering::SeqCst), 1);
    assert_eq!(foreground_calls.load(Ordering::SeqCst), 2);
    assert!(store
        .get_context_checkpoint(chat.id)
        .await
        .unwrap()
        .is_none());
    assert!(events
        .iter()
        .any(|event| matches!(event, AgentEvent::TurnCompleted { .. })));
    assert!(events
        .iter()
        .any(|event| matches!(event, AgentEvent::ContextTruncated { .. })));
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::CompactionFinished { compacted: false })),
        "a compaction that produced nothing still closes its status"
    );

    // The cache contract: everything the provider hashes before the appended
    // instruction — tools, then system, then messages — is what the step goes
    // on to send. A field that drifts here is a full-price read of the whole
    // transcript, which no test downstream would notice. Asserted as whole-
    // struct equality rather than field by field, because the hazard is a
    // *new* `ChatRequest` field the compaction call sets differently — a
    // hand-written field list cannot see one arrive.
    let requests = requests.lock().unwrap();
    assert!(is_checkpoint_request(&requests[0]));
    let (checkpoint_request, step) = (&requests[0], &requests[1]);
    assert!(!is_checkpoint_request(step));
    let mut expected = step.clone();
    expected.messages.push(ChatMessage::text(
        Role::User,
        CONTEXT_CHECKPOINT_INSTRUCTION,
    ));
    // The output cap is the one deliberate difference: it is not part of the
    // hashed prefix, so the checkpoint may size it for its own payload.
    expected.max_tokens = Some(CONTEXT_CHECKPOINT_MAX_OUTPUT_TOKENS);
    // Named separately only because whole-struct inequality reads poorly on
    // the two fields most likely to break: the tool array gates the entire
    // cache, and `OneShot` reads as natural for a maintenance call but would
    // silently discard the saving this whole design exists for.
    assert_eq!(checkpoint_request.tools, step.tools);
    assert_eq!(checkpoint_request.prompt_cache, step.prompt_cache);
    assert_eq!(*checkpoint_request, expected);
}

/// The compaction call inherits the chat's reasoning, and thinking tokens bill
/// against `max_tokens`. A cap sized for the payload alone would let thinking
/// eat it, stopping the answer mid-JSON at `MaxTokens` — which parses as
/// nothing, so the chat would never compact while paying for a whole-transcript
/// call every time the boundary advanced. The cap is bounded from the other
/// side too: a chat whose own `max_tokens` is lower sends that, because a model
/// declaring a lower output ceiling rejects the larger request outright and
/// fail-open would hide it.
#[tokio::test]
async fn the_checkpoint_output_cap_leaves_room_for_the_chat_s_reasoning() {
    let (store, chat, _workspace) = cancel_test_chat().await;
    append_semantic_checkpoint_history(&store, chat.id).await;
    let requests = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        Arc::new(SemanticCheckpointProvider {
            requests: requests.clone(),
            summary_calls: Arc::new(AtomicUsize::new(0)),
            foreground_calls: Arc::new(AtomicUsize::new(0)),
            malformed_summary: false,
            tool_first: false,
            checkpoint_calls_tool: false,
        }),
        Arc::new(ToolRegistry::new()),
        store.clone(),
        AgentConfig {
            model: "reasoning-model".into(),
            context_window: 3_000,
            reasoning_model: true,
            reasoning_effort: Some(crate::model::ReasoningEffort::High),
            compaction: test_compaction_policy(),
            ..Default::default()
        },
    );

    let (tx, rx) = unbounded();
    agent.run_turn(&chat, "continue", &tx).await.unwrap();
    drop(tx);
    let _: Vec<_> = rx.collect().await;

    {
        let requests = requests.lock().unwrap();
        let checkpoint_request = requests
            .iter()
            .find(|request| is_checkpoint_request(request))
            .expect("the transcript crossed the threshold and compacted");
        // The reasoning parameters ride along deliberately: nulling them would
        // break the byte-identical prefix the whole design depends on, so the
        // cap is what has to absorb them.
        assert!(checkpoint_request.reasoning_model);
        assert_eq!(
            checkpoint_request.reasoning_effort,
            Some(crate::model::ReasoningEffort::High)
        );
        // A conforming payload may legally reach MAX_CONTEXT_CHECKPOINT_BYTES
        // of JSON. At a conservative three bytes per token that is the floor
        // the cap must clear before a single thinking token is spent, so it has
        // to clear it with room over — unused output tokens cost nothing, and
        // `max_tokens` is outside the hashed prefix, so there is nothing to
        // trade against.
        let payload_tokens = (crate::MAX_CONTEXT_CHECKPOINT_BYTES / 3) as u32;
        assert!(
            checkpoint_request
                .max_tokens
                .is_some_and(|cap| cap >= payload_tokens * 2),
            "the checkpoint cap {:?} leaves no reasoning allowance over a {payload_tokens}-token payload",
            checkpoint_request.max_tokens,
        );
    }

    // The other direction: generous is free to bill but not free to *send*.
    // Custom and gateway models declare output ceilings well below this one and
    // reject a request that exceeds theirs before writing a token — a rejection
    // fail-open swallows, leaving a chat that silently never compacts. So the
    // checkpoint's own cap is a ceiling, clamped to the chat's when it has one.
    let (store, chat, _workspace) = cancel_test_chat().await;
    append_semantic_checkpoint_history(&store, chat.id).await;
    let requests = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        Arc::new(SemanticCheckpointProvider {
            requests: requests.clone(),
            summary_calls: Arc::new(AtomicUsize::new(0)),
            foreground_calls: Arc::new(AtomicUsize::new(0)),
            malformed_summary: false,
            tool_first: false,
            checkpoint_calls_tool: false,
        }),
        Arc::new(ToolRegistry::new()),
        store.clone(),
        AgentConfig {
            model: "low-output-ceiling-model".into(),
            context_window: 3_000,
            max_tokens: Some(8_192),
            compaction: test_compaction_policy(),
            ..Default::default()
        },
    );

    let (tx, rx) = unbounded();
    agent.run_turn(&chat, "continue", &tx).await.unwrap();
    drop(tx);
    let _: Vec<_> = rx.collect().await;

    let requests = requests.lock().unwrap();
    let checkpoint_request = requests
        .iter()
        .find(|request| is_checkpoint_request(request))
        .expect("the transcript crossed the threshold and compacted");
    assert_eq!(checkpoint_request.max_tokens, Some(8_192));
}

/// The compaction call carries the conversation's whole tool array and sends no
/// `tool_choice`, so a model may answer it with a tool call. Nothing may run:
/// the host asked for a summary, not for work, and a maintenance call is not a
/// step the turn can dispatch from.
#[tokio::test]
async fn a_checkpoint_answered_with_a_tool_call_runs_nothing_and_fails_open() {
    let (store, chat, _workspace) = cancel_test_chat().await;
    append_semantic_checkpoint_history(&store, chat.id).await;
    let summary_calls = Arc::new(AtomicUsize::new(0));
    let foreground_calls = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        Arc::new(SemanticCheckpointProvider {
            requests: requests.clone(),
            summary_calls: summary_calls.clone(),
            foreground_calls: foreground_calls.clone(),
            malformed_summary: false,
            tool_first: false,
            checkpoint_calls_tool: true,
        }),
        Arc::new(ToolRegistry::new().with(Box::new(CheckpointNoopTool))),
        store.clone(),
        AgentConfig {
            model: "small-context-model".into(),
            context_window: 3_000,
            compaction: test_compaction_policy(),
            ..Default::default()
        },
    );

    let (tx, rx) = unbounded();
    agent.run_turn(&chat, "continue", &tx).await.unwrap();
    drop(tx);
    let events = rx.collect::<Vec<_>>().await;

    // The premise, not a detail: a tool call is only possible because the
    // maintenance request advertises the conversation's tools and constrains
    // nothing. Drop the tools from it and this test keeps passing while
    // covering nothing.
    {
        let requests = requests.lock().unwrap();
        let checkpoint_request = requests
            .iter()
            .find(|request| is_checkpoint_request(request))
            .expect("the transcript crossed the threshold and compacted");
        assert!(
            !checkpoint_request.tools.is_empty(),
            "the checkpoint call carries the conversation's tool array"
        );
        assert!(checkpoint_request.tool_choice.is_none());
    }
    // The fence: one maintenance call for the turn, however it ended.
    assert_eq!(summary_calls.load(Ordering::SeqCst), 1);
    assert_eq!(foreground_calls.load(Ordering::SeqCst), 1);
    assert!(
        !events.iter().any(|event| matches!(
            event,
            AgentEvent::ToolCallStarted { .. } | AgentEvent::ToolCallCompleted { .. }
        )),
        "the maintenance call's tool call reached no dispatcher: {events:?}"
    );
    assert!(store
        .get_context_checkpoint(chat.id)
        .await
        .unwrap()
        .is_none());
    assert!(events
        .iter()
        .any(|event| matches!(event, AgentEvent::CompactionFinished { compacted: false })));
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::TurnCompleted { .. })),
        "the foreground turn still answers on deterministic reduction"
    );
}

/// The trigger is a fraction of the model's window, so the same history that
/// compacts on a small model has to pass untouched on a large one. At 0.75 the
/// 50k window only compacts past 37 500 tokens, which this transcript is far
/// below; the 3 000 window compacts past 2 250, which it clears.
#[tokio::test]
async fn model_window_change_recalculates_the_checkpoint_threshold() {
    let (store, chat, _workspace) = cancel_test_chat().await;
    append_semantic_checkpoint_history(&store, chat.id).await;

    let large_summary_calls = Arc::new(AtomicUsize::new(0));
    let large_agent = Agent::new(
        Arc::new(SemanticCheckpointProvider {
            requests: Arc::new(Mutex::new(Vec::new())),
            summary_calls: large_summary_calls.clone(),
            foreground_calls: Arc::new(AtomicUsize::new(0)),
            malformed_summary: false,
            tool_first: false,
            checkpoint_calls_tool: false,
        }),
        Arc::new(ToolRegistry::new()),
        store.clone(),
        AgentConfig {
            model: "large-context-model".into(),
            context_window: 50_000,
            compaction: test_compaction_policy(),
            ..Default::default()
        },
    );
    let (tx, rx) = unbounded();
    large_agent
        .run_turn(&chat, "large-window turn", &tx)
        .await
        .unwrap();
    drop(tx);
    let _: Vec<_> = rx.collect().await;
    assert_eq!(large_summary_calls.load(Ordering::SeqCst), 0);
    assert!(store
        .get_context_checkpoint(chat.id)
        .await
        .unwrap()
        .is_none());

    let small_requests = Arc::new(Mutex::new(Vec::new()));
    let small_summary_calls = Arc::new(AtomicUsize::new(0));
    let small_agent = Agent::new(
        Arc::new(SemanticCheckpointProvider {
            requests: small_requests.clone(),
            summary_calls: small_summary_calls.clone(),
            foreground_calls: Arc::new(AtomicUsize::new(0)),
            malformed_summary: false,
            tool_first: false,
            checkpoint_calls_tool: false,
        }),
        Arc::new(ToolRegistry::new()),
        store.clone(),
        AgentConfig {
            model: "small-context-model".into(),
            context_window: 3_000,
            compaction: test_compaction_policy(),
            ..Default::default()
        },
    );
    let (tx, rx) = unbounded();
    small_agent
        .run_turn(&chat, "small-window turn", &tx)
        .await
        .unwrap();
    drop(tx);
    let _: Vec<_> = rx.collect().await;
    assert_eq!(small_summary_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        small_requests.lock().unwrap()[0].model,
        "small-context-model",
        "compaction runs on the conversation's model, whose cache it reads"
    );
    assert_eq!(
        store
            .get_context_checkpoint(chat.id)
            .await
            .unwrap()
            .unwrap()
            .chat_id,
        chat.id
    );
}

#[tokio::test]
async fn oversized_transcript_emits_context_truncated() {
    let (store, chat, _workspace) = cancel_test_chat().await;

    // Records what the provider actually received, and answers immediately.
    struct AnswerProvider {
        seen_tokens: Arc<AtomicUsize>,
    }
    #[async_trait]
    impl ModelProvider for AnswerProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("answer")
        }
        async fn stream(&self, req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            self.seen_tokens.store(
                context::estimate_transcript_tokens(&req.messages),
                Ordering::SeqCst,
            );
            Ok(stream::iter(vec![
                ProviderEvent::TextDelta { text: "ok".into() },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ])
            .boxed())
        }
    }

    let seen_tokens = Arc::new(AtomicUsize::new(0));
    // A small context window forces reduction of a large input.
    let context_window = 3000;
    let agent = Agent::new(
        Arc::new(AnswerProvider {
            seen_tokens: seen_tokens.clone(),
        }),
        Arc::new(ToolRegistry::new()),
        store,
        AgentConfig {
            model: "answer".into(),
            context_window,
            ..Default::default()
        },
    );

    let huge = "word ".repeat(2000); // ~3300 tokens, over the ~2250 budget
    let (tx, rx) = unbounded();
    agent.run_turn(&chat, &huge, &tx).await.unwrap();
    drop(tx);
    let events: Vec<AgentEvent> = rx.collect().await;

    let truncated = events.iter().find_map(|e| match e {
        AgentEvent::ContextTruncated {
            original_tokens,
            fitted_tokens,
        } => Some((*original_tokens, *fitted_tokens)),
        _ => None,
    });
    let (original, fitted) = truncated.expect("ContextTruncated emitted for oversized input");
    assert!(
        fitted < original,
        "fitted {fitted} should be < original {original}"
    );
    // What actually went to the provider matches the reported fitted size and
    // is within the reduced budget.
    assert_eq!(seen_tokens.load(Ordering::SeqCst), fitted as usize);
    assert!(fitted as usize <= context::compute_message_budget(context_window, 0, None, &[]));
}

/// Compaction is a soft load boundary, not a last-resort fallback: once a
/// checkpoint covers a prefix, the model reads the checkpoint instead of that
/// prefix on every subsequent turn, however much window is available. Only a
/// boundary this transcript cannot locate falls back to the full raw history.
#[tokio::test]
async fn projects_a_checkpoint_whenever_its_boundary_is_valid() {
    struct CaptureProvider {
        requests: Arc<Mutex<Vec<ChatRequest>>>,
    }

    #[async_trait]
    impl ModelProvider for CaptureProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("checkpoint-capture")
        }

        async fn stream(&self, request: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            self.requests.lock().unwrap().push(request);
            Ok(stream::iter(vec![
                ProviderEvent::TextDelta { text: "ok".into() },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ])
            .boxed())
        }
    }

    let (store, chat, _workspace) = cancel_test_chat().await;
    let historical = Message {
        id: MessageId::new(),
        chat_id: chat.id,
        turn_id: TurnId::new(),
        role: Role::User,
        reasoning: Default::default(),
        content: "old decision ".repeat(1_000),
        llm_content: None,
        created_at: Utc::now(),
    };
    store.append_message(&historical).await.unwrap();
    let checkpoint = ContextCheckpoint {
        chat_id: chat.id,
        source_message_id: historical.id,
        format_version: crate::CONTEXT_CHECKPOINT_FORMAT_V1,
        content: "The user chose the durable option.".into(),
        usage: Usage::default(),
        created_at: Utc::now(),
    };
    store.save_context_checkpoint(&checkpoint).await.unwrap();

    let requests = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        Arc::new(CaptureProvider {
            requests: requests.clone(),
        }),
        Arc::new(ToolRegistry::new()),
        store.clone(),
        AgentConfig {
            model: "checkpoint-capture".into(),
            context_window: 2_000,
            ..Default::default()
        },
    );
    let (tx, rx) = unbounded();
    agent
        .run_turn(&chat, "What did we decide?", &tx)
        .await
        .unwrap();
    drop(tx);
    let events = rx.collect::<Vec<_>>().await;

    let request = requests
        .lock()
        .unwrap()
        .first()
        .cloned()
        .expect("one provider request");
    let projected: Vec<_> = request
            .messages
            .iter()
            .filter(|message| {
                message.role == Role::System
                    && message.content.iter().any(|block| {
                        matches!(block, ContentBlock::Text { text } if text.contains(CHECKPOINT_CONTEXT_PREFIX))
                    })
            })
            .collect();
    assert_eq!(
        projected.len(),
        1,
        "the checkpoint is projected exactly once"
    );
    assert!(projected[0].content.iter().any(
        |block| matches!(block, ContentBlock::Text { text } if text.contains(&checkpoint.content)),
    ));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::ContextTruncated { .. })),
        "a projected checkpoint with a tail that fits has not truncated anything"
    );
    assert!(store
        .list_messages(chat.id)
        .await
        .unwrap()
        .iter()
        .all(|message| !message.content.contains(CHECKPOINT_CONTEXT_PREFIX)));
    assert!(!format!("{events:?}").contains(CHECKPOINT_CONTEXT_PREFIX));

    // A window large enough for the raw covered history changes nothing: the
    // boundary is still valid, so the prefix stays replaced by its checkpoint.
    let requests = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        Arc::new(CaptureProvider {
            requests: requests.clone(),
        }),
        Arc::new(ToolRegistry::new()),
        store,
        AgentConfig {
            model: "checkpoint-capture".into(),
            context_window: 50_000,
            ..Default::default()
        },
    );
    let (tx, rx) = unbounded();
    agent
        .run_turn(&chat, "Please answer again.", &tx)
        .await
        .unwrap();
    drop(tx);
    let _: Vec<AgentEvent> = rx.collect().await;
    let wide = requests
        .lock()
        .unwrap()
        .first()
        .cloned()
        .expect("one provider request");
    let mentions = |needle: &str| {
        wide.messages.iter().any(|message| {
            message
                .content
                .iter()
                .any(|block| matches!(block, ContentBlock::Text { text } if text.contains(needle)))
        })
    };
    assert!(
        mentions(CHECKPOINT_CONTEXT_PREFIX),
        "a valid boundary projects regardless of how much window is spare"
    );
    assert!(
        !mentions("old decision"),
        "the covered prefix is not resent beside its own checkpoint"
    );

    // Fail open: a checkpoint whose source row this transcript cannot locate
    // has no boundary to stand in for, so the raw history goes out whole.
    let raw = vec![ChatMessage::text(Role::User, "What did we decide?")];
    assert_eq!(
        agent.fit_transcript(&raw, 0, Some(&checkpoint), None),
        (raw.clone(), false)
    );
}

#[tokio::test]
async fn checkpoint_fitting_preserves_tool_pairs_and_fails_closed_when_over_budget() {
    let (store, chat, _workspace) = cancel_test_chat().await;
    let config = AgentConfig {
        model: "checkpoint-fit".into(),
        context_window: 1_400,
        ..Default::default()
    };
    let agent = Agent::new(
        Arc::new(FakeProvider {
            calls: AtomicUsize::new(0),
        }),
        Arc::new(ToolRegistry::new()),
        store,
        config.clone(),
    );
    let transcript = vec![
        ChatMessage::text(Role::User, "old detail ".repeat(1_000)),
        ChatMessage {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "call_1".into(),
                name: "read_file".into(),
                input: serde_json::json!({"path": "decision.md"}),
            }],
            reasoning: MessageReasoning::default(),
        },
        ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "call_1".into(),
                content: "the durable decision".into(),
                is_error: false,
            }],
            reasoning: MessageReasoning::default(),
        },
        ChatMessage::text(Role::User, "Continue from the decision."),
    ];
    let checkpoint = ContextCheckpoint {
        chat_id: chat.id,
        source_message_id: MessageId::new(),
        format_version: crate::CONTEXT_CHECKPOINT_FORMAT_V1,
        content: "Earlier discussion selected the durable option.".into(),
        usage: Usage::default(),
        created_at: Utc::now(),
    };
    let (fitted, reduced) = agent.fit_transcript(&transcript, 0, Some(&checkpoint), Some(1));
    assert!(
        !reduced,
        "the post-boundary tail fits beside the checkpoint, so nothing was trimmed"
    );
    assert!(matches!(
        fitted.first(),
        Some(ChatMessage {
            role: Role::System,
            ..
        })
    ));
    assert!(!context::has_orphaned_tool_blocks(&fitted));
    assert!(fitted.iter().any(|message| message
        .content
        .iter()
        .any(|block| matches!(block, ContentBlock::ToolUse { id, .. } if id == "call_1"),)));
    assert!(fitted.iter().any(|message| message.content.iter().any(
            |block| matches!(block, ContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == "call_1"),
        )));

    let over_budget = ContextCheckpoint {
        content: "x".repeat(crate::MAX_CONTEXT_CHECKPOINT_BYTES),
        ..checkpoint
    };
    let expected = context::fit_to_budget(
        &transcript[1..],
        context::compute_message_budget(config.context_window, 0, None, &[]),
        context::content_floor_for_level(0),
    );
    assert_eq!(
        agent.fit_transcript(&transcript, 0, Some(&over_budget), Some(1)),
        expected,
        "a checkpoint that cannot share the request budget is dropped rather than \
         crowding out the post-boundary history it was meant to summarize"
    );
}

#[test]
fn unsupported_or_foreign_checkpoints_are_not_projectable() {
    let chat_id = SessionId::new();
    let checkpoint = ContextCheckpoint {
        chat_id,
        source_message_id: MessageId::new(),
        format_version: crate::CONTEXT_CHECKPOINT_FORMAT_V1,
        content: "valid historical context".into(),
        usage: Usage::default(),
        created_at: Utc::now(),
    };
    assert!(checkpoint_is_projectable(&checkpoint, chat_id));
    assert!(!checkpoint_is_projectable(&checkpoint, SessionId::new()));
    assert!(checkpoint_is_projectable(
        &ContextCheckpoint {
            format_version: crate::CONTEXT_CHECKPOINT_FORMAT_V2,
            ..checkpoint.clone()
        },
        chat_id,
    ));
    assert!(!checkpoint_is_projectable(
        &ContextCheckpoint {
            format_version: crate::CONTEXT_CHECKPOINT_FORMAT_V2 + 1,
            ..checkpoint
        },
        chat_id,
    ));
}
