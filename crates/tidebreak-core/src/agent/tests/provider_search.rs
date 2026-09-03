use super::*;

struct ProviderSearchWithStandaloneControlProvider {
    calls: AtomicUsize,
    name: &'static str,
    arguments: &'static str,
}

#[async_trait]
impl ModelProvider for ProviderSearchWithStandaloneControlProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("provider-search-with-standalone-control")
    }

    async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        let events = if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            vec![
                ProviderEvent::ProviderExecutedToolCall {
                    name: crate::WEB_SEARCH_TOOL.into(),
                    input: serde_json::json!({"query": "tidebreak release notes"}),
                    output: serde_json::json!({
                        "provider": "anthropic",
                        "results": [{
                            "url": "https://www.example.com/notes",
                            "title": "Release notes",
                            "snippet": "what shipped",
                        }],
                    }),
                    is_error: false,
                    replay: None,
                },
                ProviderEvent::ToolCallStarted {
                    index: 0,
                    id: "standalone_1".into(),
                    name: self.name.into(),
                },
                ProviderEvent::ToolCallArgsDelta {
                    index: 0,
                    fragment: self.arguments.into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::ToolUse,
                },
            ]
        } else {
            vec![
                ProviderEvent::TextDelta {
                    text: "I reissued no mixed control call.".into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ]
        };
        Ok(stream::iter(events).boxed())
    }
}

async fn drive_provider_search_with_standalone_control(
    name: &'static str,
    arguments: &'static str,
) -> (AgentTurnOutcome, Vec<AgentEvent>) {
    let db = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect_test_sqlite_fixture(&format!(
            "sqlite://{}?mode=rwc",
            db.path().join("provider-search-sibling.db").display()
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
        .accept_turn(turn_id, chat.id, "fake", "use the control if needed")
        .await
        .unwrap();
    let lease_token = uuid::Uuid::new_v4();
    let now = Utc::now();
    store
        .claim_turn(lease_token, now, now + chrono::Duration::minutes(1))
        .await
        .unwrap();

    let mut registry = ToolRegistry::new().with(Box::new(HostWebSearch));
    match name {
        crate::ASK_USER_QUESTIONS_TOOL => registry.register_validated_foreground_client(
            crate::ask_user_questions_tool_spec(),
            ApprovalClass::ReadOnly,
            crate::validate_ask_user_questions_arguments,
        ),
        crate::SPAWN_SANDBOX_AGENT_TOOL | crate::WAIT_FOR_AGENTS_TOOL => {
            registry.register_foreground_agent_orchestration();
        }
        crate::agent_tools::REPORT_BLOCKED_TOOL => registry.register_validated_foreground_client(
            crate::agent_tools::report_blocked_tool_spec(),
            ApprovalClass::ReadOnly,
            crate::agent_tools::validate_report_blocked_arguments,
        ),
        _ => panic!("unsupported standalone control {name}"),
    }

    let provider = Arc::new(ProviderSearchWithStandaloneControlProvider {
        calls: AtomicUsize::new(0),
        name,
        arguments,
    });
    let agent = Agent::new(
        provider.clone(),
        Arc::new(registry),
        store.clone(),
        AgentConfig {
            model: "fake".into(),
            max_steps: 2,
            web_search: TurnWebSearch::Vendor(VendorWebSearch { max_uses: 1 }),
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

    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    assert!(
        store.list_tool_calls(chat.id).await.unwrap().is_empty(),
        "neither the rejected control nor its provider-native sibling may be durable"
    );
    assert!(
        !events.iter().any(|event| matches!(
            event,
            AgentEvent::ToolCallStarted { name, .. } if name == crate::WEB_SEARCH_TOOL
        )),
        "a rejected mixed step must not publish a durable native-search activity: {events:?}"
    );
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ToolCallCompleted { output, .. }
            if output.is_error && output.content.contains("must be called alone")
    )));
    (outcome, events)
}

#[tokio::test]
async fn provider_native_search_cannot_sibling_foreground_checkpoint_controls() {
    for (name, arguments) in [
        (
            crate::ASK_USER_QUESTIONS_TOOL,
            r#"{"questions":[{"id":"target","header":"Target","question":"Where should I deploy?","options":[{"id":"staging","label":"Staging","description":"Deploy for verification."}]}]}"#,
        ),
        (
            crate::SPAWN_SANDBOX_AGENT_TOOL,
            r#"{"task":"Research the release notes."}"#,
        ),
        (
            crate::WAIT_FOR_AGENTS_TOOL,
            r#"{"agent_ids":["00000000-0000-0000-0000-000000000001"]}"#,
        ),
    ] {
        let (outcome, _) = drive_provider_search_with_standalone_control(name, arguments).await;
        assert!(
            matches!(
                &outcome,
                AgentTurnOutcome::Completed {
                    output,
                    stop_reason: StopReason::EndTurn,
                    refusal: None,
                    model_steps: 2,
                    ..
                } if output.content == "I reissued no mixed control call."
            ),
            "{name} must be rejected before it can checkpoint: {outcome:?}"
        );
    }
}

#[tokio::test]
async fn provider_native_search_cannot_sibling_report_blocked() {
    let (outcome, _) = drive_provider_search_with_standalone_control(
        crate::agent_tools::REPORT_BLOCKED_TOOL,
        r#"{"reason_code":"required_source_missing","explanation":"The mandatory source is unavailable."}"#,
    )
    .await;
    assert!(
        matches!(
            &outcome,
            AgentTurnOutcome::Completed {
                output,
                stop_reason: StopReason::EndTurn,
                refusal: None,
                model_steps: 2,
                ..
            } if output.content == "I reissued no mixed control call."
        ),
        "report_blocked must not terminalize a mixed provider-native step: {outcome:?}"
    );
}

/// The host's own `web_search`. Registered so the turn has one to withhold,
/// and never expected to run.
struct HostWebSearch;

#[async_trait]
impl Tool for HostWebSearch {
    fn spec(&self) -> ToolSpec {
        crate::web_search_tool_spec()
    }
    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::Sensitive
    }
    async fn execute(&self, _ctx: &ToolCtx, _args: Value) -> Result<ToolOutput> {
        panic!("a search the provider already ran must never be dispatched")
    }
}

/// What one request advertised, and the vendor budget it carried.
type SearchSurfaces = Arc<Mutex<Vec<(Vec<String>, Option<VendorWebSearch>)>>>;

/// Answers with a search it already ran, recording what each request
/// advertised and asked for.
struct VendorSearchProvider {
    seen: SearchSurfaces,
}

/// Spends part of a turn-level vendor-search allowance before asking the host
/// to run a tool, forcing a second foreground model request.
struct MultiStepVendorSearchBudgetProvider {
    calls: AtomicUsize,
    seen: Arc<Mutex<Vec<ChatRequest>>>,
}

/// Reports provider-native browsing actions through the shared search name.
/// None of these inputs can be represented by Tidebreak's canonical
/// `web_search` arguments, so they must remain transient provider state.
struct MalformedVendorSearchProvider;

/// Interleaves ordinary host calls around one provider-executed search, then
/// records the resumed request so live and rebuilt transcript order can be
/// compared against the original event stream.
struct InterleavedProviderSearchProvider {
    calls: AtomicUsize,
    seen: Arc<Mutex<Vec<ChatRequest>>>,
}

#[async_trait]
impl ModelProvider for VendorSearchProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("fake")
    }

    async fn stream(&self, req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        self.seen.lock().unwrap().push((
            req.tools.iter().map(|tool| tool.name.clone()).collect(),
            req.vendor_web_search,
        ));
        Ok(stream::iter(vec![
            ProviderEvent::ProviderExecutedToolCall {
                name: crate::WEB_SEARCH_TOOL.into(),
                input: serde_json::json!({ "query": "tidebreak release notes" }),
                output: serde_json::json!({
                    "provider": "anthropic",
                    "results": [{
                        "url": "https://www.example.com/notes",
                        "title": "Release notes",
                        "snippet": "what shipped",
                    }],
                }),
                is_error: false,
                replay: None,
            },
            ProviderEvent::TextDelta {
                text: "here is what I found".into(),
            },
            ProviderEvent::Stop {
                reason: StopReason::EndTurn,
            },
        ])
        .boxed())
    }
}

#[async_trait]
impl ModelProvider for MultiStepVendorSearchBudgetProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("multi-step-vendor-search-budget")
    }

    async fn stream(&self, req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        self.seen.lock().unwrap().push(req);
        let events = if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            vec![
                ProviderEvent::ProviderExecutedToolCall {
                    name: crate::WEB_SEARCH_TOOL.into(),
                    input: serde_json::json!({"query": "first canonical search"}),
                    output: serde_json::json!({
                        "provider": "anthropic",
                        "results": [{
                            "url": "https://example.com/first",
                            "title": "First result",
                            "snippet": "Canonical provider evidence",
                        }],
                    }),
                    is_error: false,
                    replay: None,
                },
                // Receipt admission rejects this malformed evidence, but the
                // provider still reported executing a search and therefore
                // spent one unit of the turn-level allowance.
                ProviderEvent::ProviderExecutedToolCall {
                    name: crate::WEB_SEARCH_TOOL.into(),
                    input: serde_json::json!({"query": "second malformed search"}),
                    output: serde_json::json!({"results": "not a result list"}),
                    is_error: false,
                    replay: None,
                },
                ProviderEvent::ToolCallStarted {
                    index: 0,
                    id: "checkpoint_noop_1".into(),
                    name: "checkpoint_noop".into(),
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
                    text: "done after the host-tool follow-up".into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ]
        };
        Ok(stream::iter(events).boxed())
    }
}

#[async_trait]
impl ModelProvider for MalformedVendorSearchProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("fake")
    }

    async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        let output = serde_json::json!({
            "provider": "openai",
            "results": [],
        });
        let inputs = [
            serde_json::json!({}),
            serde_json::json!({
                "query": { "type": "open_page", "url": "https://example.com" }
            }),
            serde_json::json!({
                "query": { "type": "find_in_page", "pattern": "Tidebreak" }
            }),
        ];
        let mut events = Vec::new();
        for is_error in [false, true] {
            events.extend(inputs.iter().cloned().map(|input| {
                ProviderEvent::ProviderExecutedToolCall {
                    name: crate::WEB_SEARCH_TOOL.into(),
                    input,
                    output: output.clone(),
                    is_error,
                    replay: None,
                }
            }));
        }
        events.extend([
            ProviderEvent::TextDelta {
                text: "I could not complete a canonical web search.".into(),
            },
            ProviderEvent::Stop {
                reason: StopReason::EndTurn,
            },
        ]);
        Ok(stream::iter(events).boxed())
    }
}

#[async_trait]
impl ModelProvider for InterleavedProviderSearchProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("interleaved-provider-search")
    }

    async fn stream(&self, req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        self.seen.lock().unwrap().push(req);
        let events = if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            vec![
                ProviderEvent::ToolCallStarted {
                    index: 0,
                    id: "tool_before".into(),
                    name: "tool_before".into(),
                },
                ProviderEvent::ToolCallArgsDelta {
                    index: 0,
                    fragment: "{}".into(),
                },
                ProviderEvent::ProviderExecutedToolCall {
                    name: crate::WEB_SEARCH_TOOL.into(),
                    input: serde_json::json!({"query": "Tidebreak release notes"}),
                    output: serde_json::json!({
                        "provider": "anthropic",
                        "results": [{
                            "url": "https://example.com/notes",
                            "title": "Release notes",
                            "snippet": "What shipped",
                        }],
                    }),
                    is_error: false,
                    replay: None,
                },
                ProviderEvent::ToolCallStarted {
                    index: 1,
                    id: "tool_after".into(),
                    name: "tool_after".into(),
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
            vec![
                ProviderEvent::TextDelta {
                    text: "done in provider order".into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ]
        };
        Ok(stream::iter(events).boxed())
    }
}

/// A vendor turn end to end: the model is offered one web search rather
/// than two, the search the provider ran is kept like any other tool call,
/// and a later turn replays it as an ordinary pair.
#[tokio::test]
async fn a_provider_executed_search_replaces_the_host_tool_and_is_kept_like_one() {
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
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();

    let seen = Arc::new(Mutex::new(Vec::new()));
    let budget = VendorWebSearch { max_uses: 3 };
    let agent = Agent::new(
        Arc::new(VendorSearchProvider { seen: seen.clone() }),
        Arc::new(
            ToolRegistry::new()
                .with(Box::new(HostWebSearch))
                .with(Box::new(ReadFile)),
        ),
        store.clone(),
        AgentConfig {
            model: "fake".into(),
            web_search: TurnWebSearch::Vendor(budget),
            ..Default::default()
        },
    );

    let (tx, rx) = unbounded();
    agent.run_turn(&chat, "what shipped?", &tx).await.unwrap();
    drop(tx);
    let events: Vec<AgentEvent> = rx.collect().await;

    // One capability, one name: the request carries the vendor budget and
    // withholds the host tool, while the rest of the surface is untouched.
    let requests = seen.lock().unwrap().clone();
    let (advertised, vendor) = requests.first().expect("the turn made a model call");
    assert_eq!(*vendor, Some(budget));
    assert!(
        !advertised.contains(&crate::WEB_SEARCH_TOOL.to_owned())
            && advertised.contains(&"read_file".to_owned()),
        "advertised the wrong surface: {advertised:?}"
    );

    // The reader sees the search happen and finish, exactly as they would
    // a host search.
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ToolCallStarted { name, .. } if name == crate::WEB_SEARCH_TOOL
    )));
    assert!(events
        .iter()
        .any(|event| matches!(event, AgentEvent::ToolCallCompleted { .. })));

    let calls = store.list_tool_calls(chat.id).await.unwrap();
    let [call] = &calls[..] else {
        panic!("the provider's search was not recorded: {calls:?}");
    };
    assert_eq!(call.name, crate::WEB_SEARCH_TOOL);
    assert_eq!(call.status, ToolCallStatus::Completed);
    assert_eq!(call.arguments["query"], "tidebreak release notes");
    assert!(call
        .result
        .as_deref()
        .is_some_and(|result| result.contains("https://www.example.com/notes")));

    // A later turn rebuilds it as the same provider-executed shape, so
    // adapters can origin-gate native replay or fall back to cleartext.
    let messages = store.list_messages(chat.id).await.unwrap();
    let rebuilt = rebuild_transcript(&messages, &calls, &[], DEFAULT_MAX_TOOL_RESULT_BYTES);
    let blocks: Vec<&ContentBlock> = rebuilt
        .iter()
        .flat_map(|message| message.content.iter())
        .collect();
    assert!(
        blocks.iter().any(|block| matches!(
            block,
            ContentBlock::ProviderExecutedToolCall { name, output, .. }
                if name == crate::WEB_SEARCH_TOOL
                    && output.to_string().contains("Release notes")
        )),
        "the replayed call kept no result: {rebuilt:?}"
    );
}

#[tokio::test]
async fn foreground_vendor_search_budget_is_offered_to_only_one_model_request() {
    let db = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect_test_sqlite_fixture(&format!(
            "sqlite://{}?mode=rwc",
            db.path()
                .join("multi-step-vendor-search-budget.db")
                .display()
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
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();

    let seen = Arc::new(Mutex::new(Vec::new()));
    let budget = VendorWebSearch { max_uses: 3 };
    let agent = Agent::new(
        Arc::new(MultiStepVendorSearchBudgetProvider {
            calls: AtomicUsize::new(0),
            seen: seen.clone(),
        }),
        Arc::new(ToolRegistry::new().with(Box::new(CheckpointNoopTool))),
        store,
        AgentConfig {
            model: "fake".into(),
            web_search: TurnWebSearch::Vendor(budget),
            ..Default::default()
        },
    );

    let (tx, _rx) = unbounded();
    agent
        .run_turn(&chat, "search, then use the host tool", &tx)
        .await
        .unwrap();

    let requests = seen.lock().unwrap();
    assert_eq!(requests.len(), 2, "expected one host-tool follow-up");
    assert_eq!(requests[0].vendor_web_search, Some(budget));
    assert_eq!(
        requests[1].vendor_web_search, None,
        "a later request cannot reopen any part of the per-turn allowance"
    );
}

#[tokio::test]
async fn interleaved_host_calls_and_provider_search_keep_one_order_everywhere() {
    let db = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect_test_sqlite_fixture(&format!(
            "sqlite://{}?mode=rwc",
            db.path().join("interleaved-provider-search.db").display()
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
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();

    let seen = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        Arc::new(InterleavedProviderSearchProvider {
            calls: AtomicUsize::new(0),
            seen: seen.clone(),
        }),
        Arc::new(ToolRegistry::new().with(Box::new(HostWebSearch))),
        store.clone(),
        AgentConfig {
            provider: Some(ProviderId::new("anthropic")),
            model: "claude-order".into(),
            web_search: TurnWebSearch::Vendor(VendorWebSearch { max_uses: 1 }),
            ..AgentConfig::default()
        },
    );

    let (tx, rx) = unbounded();
    agent
        .run_turn(&chat, "keep the exact tool order", &tx)
        .await
        .unwrap();
    drop(tx);
    let live_events = rx.collect::<Vec<_>>().await;
    for event in &live_events {
        store.append_event(chat.id, event).await.unwrap();
    }

    let expected = ["tool_before", crate::WEB_SEARCH_TOOL, "tool_after"]
        .map(str::to_owned)
        .to_vec();
    let started_names = |events: &[AgentEvent]| {
        events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::ToolCallStarted { name, .. } => Some(name.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(started_names(&live_events), expected);

    let journal = store.list_events(chat.id, 0).await.unwrap();
    let journal_events = journal
        .iter()
        .map(|entry| entry.event.clone())
        .collect::<Vec<_>>();
    assert_eq!(started_names(&journal_events), expected);

    let calls = store.list_tool_calls(chat.id).await.unwrap();
    assert_eq!(
        calls
            .iter()
            .map(|call| call.name.clone())
            .collect::<Vec<_>>(),
        expected,
        "durable history order diverged: {calls:?}"
    );

    let block_names = |messages: &[ChatMessage]| {
        messages
            .iter()
            .flat_map(|message| &message.content)
            .filter_map(|block| match block {
                ContentBlock::ToolUse { name, .. }
                | ContentBlock::ProviderExecutedToolCall { name, .. } => Some(name.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    {
        let requests = seen.lock().unwrap();
        assert_eq!(requests.len(), 2, "unexpected requests: {requests:?}");
        assert_eq!(
            block_names(&requests[1].messages),
            expected,
            "the live resumed transcript reordered the provider step"
        );
    }

    let messages = store.list_messages(chat.id).await.unwrap();
    let rebuilt = rebuild_transcript(&messages, &calls, &[], DEFAULT_MAX_TOOL_RESULT_BYTES);
    assert_eq!(
        block_names(&rebuilt),
        expected,
        "durable transcript rebuild reordered the provider step: {rebuilt:?}"
    );
}

#[tokio::test]
async fn malformed_provider_browsing_actions_are_never_persisted_as_host_searches() {
    let db = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect_test_sqlite_fixture(&format!(
            "sqlite://{}?mode=rwc",
            db.path().join("malformed-search.db").display()
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
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();
    let agent = Agent::new(
        Arc::new(MalformedVendorSearchProvider),
        Arc::new(ToolRegistry::new().with(Box::new(HostWebSearch))),
        store.clone(),
        AgentConfig {
            model: "fake".into(),
            web_search: TurnWebSearch::Vendor(VendorWebSearch { max_uses: 3 }),
            ..Default::default()
        },
    );

    let (tx, rx) = unbounded();
    agent
        .run_turn(&chat, "inspect the repository", &tx)
        .await
        .unwrap();
    drop(tx);
    let events: Vec<AgentEvent> = rx.collect().await;

    assert!(store.list_tool_calls(chat.id).await.unwrap().is_empty());
    assert!(!events.iter().any(|event| matches!(
        event,
        AgentEvent::ToolCallStarted { name, .. } if name == crate::WEB_SEARCH_TOOL
    )));
    assert!(!events
        .iter()
        .any(|event| matches!(event, AgentEvent::ToolCallCompleted { .. })));
}
