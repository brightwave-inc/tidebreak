use super::*;

/// Advertisement order has to depend only on which tools are registered.
/// A regression here is invisible in behavior — it shows up as prompt-cache
/// misses and irreproducible runs, so nothing else would catch it.
#[test]
fn advertised_tools_are_ordered_by_name_whatever_the_registration_order() {
    let forwards = ToolRegistry::default()
        .with(Box::new(ListDir))
        .with(Box::new(ReadFile))
        .with(Box::new(WriteFile));
    let backwards = ToolRegistry::default()
        .with(Box::new(WriteFile))
        .with(Box::new(ReadFile))
        .with(Box::new(ListDir));

    let names = |registry: &ToolRegistry| -> Vec<String> {
        registry.specs().into_iter().map(|spec| spec.name).collect()
    };
    assert_eq!(names(&forwards), ["list_dir", "read_file", "write_file"]);
    assert_eq!(names(&forwards), names(&backwards));
}

#[test]
fn tool_arguments_are_parsed_without_forgiving_malformed_json() {
    assert_eq!(parse_tool_args(""), Some(Value::Object(Default::default())));
    assert_eq!(
        parse_tool_args(r#"{"hint":"Documents"}"#),
        Some(serde_json::json!({"hint": "Documents"}))
    );
    assert_eq!(parse_tool_args(r#"{"hint":"Documents""#), None);
}

#[test]
fn malformed_arguments_keep_the_streamed_fragment_beside_the_coerced_object() {
    assert_eq!(parse_args(""), (Value::Object(Default::default()), None));
    assert_eq!(
        parse_args(r#"{"hint":"Documents"}"#),
        (serde_json::json!({"hint": "Documents"}), None)
    );
    let (value, fragment) = parse_args(r#"{"hint":"Documents""#);
    assert_eq!(value, Value::Object(Default::default()));
    assert_eq!(fragment.as_deref(), Some(r#"{"hint":"Documents""#));
    // The fragment is bounded, and the bound lands on a char boundary.
    let mut huge = String::from(r#"{"hint":""#);
    huge.push_str(&"é".repeat(ToolCallRecord::MAX_ARGUMENT_BYTES));
    let (_, fragment) = parse_args(&huge);
    let fragment = fragment.expect("a garbled stream keeps its fragment");
    assert!(fragment.len() <= ToolCallRecord::MAX_ARGUMENT_BYTES);
}

/// A large result used to be cut to the feedback budget *before* it was
/// written down, so the remainder was destroyed rather than withheld and
/// the record's own 512 KiB cap was unreachable. Storage and context budget
/// are different questions and now have different bounds.
#[test]
fn a_large_result_is_kept_whole_in_the_record_and_cut_only_for_the_model() {
    let feedback = DEFAULT_MAX_TOOL_RESULT_BYTES;
    let durable = crate::model::ToolCallRecord::MAX_RESULT_BYTES;
    assert!(
        durable > feedback,
        "the record must hold more than one turn feeds"
    );

    // Bigger than the feedback budget, smaller than the record's cap: this
    // is the whole class of result that used to lose its tail.
    let content = "x".repeat(feedback * 2);
    assert!(content.len() < durable);
    assert_eq!(truncate_to_bytes(&content, durable, None), None);

    let call_id = CallId::new();
    let for_model =
        truncate_to_bytes(&content, feedback, Some(call_id)).expect("exceeds the budget");
    assert!(for_model.len() < content.len());
    assert!(for_model.contains("[truncated:"));
    assert!(for_model.contains(&content.len().to_string()));
    // The notice names the call, so the cut is a next step rather than a
    // dead end.
    assert!(for_model.contains("read_tool_result"));
    assert!(for_model.contains(&call_id.to_string()));
}

#[test]
fn exec_preview_blocks_follow_result_text_and_respect_model_capability() {
    let image = ImageRef {
        blob_id: uuid::Uuid::from_u128(7),
        media_type: crate::ImageMediaType::Png,
        width: 400,
        height: 300,
        byte_len: 10,
    };
    let visual = tool_result_blocks("call".into(), "done".into(), false, &[image], true);
    assert!(matches!(
        &visual[..],
        [
            ContentBlock::ToolResult { content, .. },
            ContentBlock::Image { image: attached }
        ] if content.contains("attached below") && *attached == image
    ));

    let text_only = tool_result_blocks("call".into(), "done".into(), false, &[image], false);
    assert!(matches!(
        &text_only[..],
        [ContentBlock::ToolResult { content, .. }]
            if content.contains("selected model does not accept image input")
    ));
}

/// One `read_file` call per step, arguments taken from a script, then a
/// final answer once the script runs out.
struct RepeatedCallProvider {
    calls: AtomicUsize,
    scripts: Vec<&'static str>,
}

#[async_trait]
impl ModelProvider for RepeatedCallProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("repeat")
    }

    async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        let step = self.calls.fetch_add(1, Ordering::SeqCst);
        let events = match self.scripts.get(step) {
            Some(args) => vec![
                ProviderEvent::ToolCallStarted {
                    index: 0,
                    id: format!("call_{step}"),
                    name: "read_file".into(),
                },
                ProviderEvent::ToolCallArgsDelta {
                    index: 0,
                    fragment: (*args).into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::ToolUse,
                },
            ],
            None => vec![
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

/// A read-only tool that counts its executions, so a test can tell a call
/// that ran from one that was answered without running.
struct CountingReadTool {
    ran: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for CountingReadTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "read_file".into(),
            description: "a counting read tool".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::ReadOnly
    }

    async fn execute(&self, _ctx: &ToolCtx, _args: Value) -> Result<ToolOutput> {
        self.ran.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutput::text("same result"))
    }
}

fn repeated_call_agent(
    store: Arc<dyn Store>,
    ran: Arc<AtomicUsize>,
    scripts: Vec<&'static str>,
) -> Agent {
    Agent::new(
        Arc::new(RepeatedCallProvider {
            calls: AtomicUsize::new(0),
            scripts,
        }),
        Arc::new(ToolRegistry::new().with(Box::new(CountingReadTool { ran }))),
        store,
        AgentConfig {
            model: "fake".into(),
            ..Default::default()
        },
    )
}

/// After `REPEATED_CALL_LIMIT` identical executions, further identical
/// calls are answered without dispatching the tool — and the refusal still
/// terminalizes the admitted durable row, so recovery never finds a
/// refused call pending.
#[tokio::test]
async fn the_fourth_identical_call_is_refused_instead_of_run() {
    let store = search_grant_store().await;
    let chat = search_grant_chat(&store).await;

    let ran = Arc::new(AtomicUsize::new(0));
    let same = r#"{"path":"note.txt"}"#;
    let agent = repeated_call_agent(store.clone(), ran.clone(), vec![same; 5]);

    let (tx, rx) = unbounded();
    agent.run_turn(&chat, "go", &tx).await.unwrap();
    drop(tx);
    let events: Vec<AgentEvent> = rx.collect().await;

    assert!(
        matches!(events.last(), Some(AgentEvent::TurnCompleted { .. })),
        "the refusal steers the model, it does not fail the turn: {events:?}"
    );
    assert_eq!(
        ran.load(Ordering::SeqCst),
        REPEATED_CALL_LIMIT,
        "only the streak executes; every later identical call is refused"
    );

    let calls = store.list_tool_calls(chat.id).await.unwrap();
    assert_eq!(calls.len(), 5, "refused calls still get durable rows");
    for call in &calls[..REPEATED_CALL_LIMIT] {
        assert_eq!(call.status, ToolCallStatus::Completed);
    }
    // The fourth and fifth asks are both refused: re-issuing the same
    // call keeps getting the refusal until something changes.
    for call in &calls[REPEATED_CALL_LIMIT..] {
        assert_eq!(call.status, ToolCallStatus::Failed);
        assert!(
            call.result
                .as_deref()
                .is_some_and(|result| result.starts_with("not run: this exact call")),
            "the refusal is the model-facing result: {:?}",
            call.result
        );
        assert!(call.resolved_at.is_some(), "the refused row terminalizes");
    }
}

/// A different argument is a change of course: it executes, and the
/// original call earns a fresh streak afterwards.
#[tokio::test]
async fn a_changed_argument_resets_the_repeat_streak() {
    let store = search_grant_store().await;
    let chat = search_grant_chat(&store).await;

    let ran = Arc::new(AtomicUsize::new(0));
    let same = r#"{"path":"note.txt"}"#;
    let other = r#"{"path":"other.txt"}"#;
    let agent = repeated_call_agent(
        store.clone(),
        ran.clone(),
        vec![same, same, same, other, same],
    );

    let (tx, rx) = unbounded();
    agent.run_turn(&chat, "go", &tx).await.unwrap();
    drop(tx);
    let events: Vec<AgentEvent> = rx.collect().await;

    assert!(matches!(
        events.last(),
        Some(AgentEvent::TurnCompleted { .. })
    ));
    assert_eq!(ran.load(Ordering::SeqCst), 5, "every call ran");
    let calls = store.list_tool_calls(chat.id).await.unwrap();
    assert!(
        calls
            .iter()
            .all(|call| call.status == ToolCallStatus::Completed),
        "nothing was refused: {calls:?}"
    );
}

#[tokio::test]
async fn large_tool_results_are_truncated() {
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("note.txt"), "x".repeat(10_000)).unwrap();
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
        Arc::new(FakeProvider {
            calls: AtomicUsize::new(0),
        }),
        Arc::new(ToolRegistry::new().with(Box::new(ReadFile))),
        store,
        AgentConfig {
            model: "fake".into(),
            max_tool_result_bytes: 100,
            tool_scratch: Some(tool_scratch(workspace.path())),
            ..Default::default()
        },
    );

    let (tx, rx) = unbounded();
    agent.run_turn(&chat, "read note.txt", &tx).await.unwrap();
    drop(tx);
    let events: Vec<AgentEvent> = rx.collect().await;

    let output = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::ToolCallCompleted { output, .. } => Some(output.clone()),
            _ => None,
        })
        .expect("a tool completed");
    assert!(!output.is_error);
    assert!(output.content.len() < 10_000, "result should be capped");
    assert!(output.content.contains("[truncated:"));
}

/// Streams `counter` calls whose arguments are well-formed JSON: first a
/// shape the advertised schema forbids, then a conforming one.
struct SchemaArgsProvider {
    calls: AtomicUsize,
}

#[async_trait]
impl ModelProvider for SchemaArgsProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("schema-args")
    }
    async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        let events = match self.calls.fetch_add(1, Ordering::SeqCst) {
            0 => vec![
                ProviderEvent::ToolCallStarted {
                    index: 0,
                    id: "call_wrong".into(),
                    name: "strict_counter".into(),
                },
                ProviderEvent::ToolCallArgsDelta {
                    index: 0,
                    fragment: r#"{"path": 42}"#.into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::ToolUse,
                },
            ],
            1 => vec![
                ProviderEvent::ToolCallStarted {
                    index: 0,
                    id: "call_right".into(),
                    name: "strict_counter".into(),
                },
                ProviderEvent::ToolCallArgsDelta {
                    index: 0,
                    fragment: r#"{"path": "note"}"#.into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::ToolUse,
                },
            ],
            _ => vec![
                ProviderEvent::TextDelta { text: "ok".into() },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ],
        };
        Ok(stream::iter(events).boxed())
    }
}

/// A read-only tool with a required, typed argument.
struct StrictCountingTool {
    ran: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for StrictCountingTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "strict_counter".into(),
            description: "a read-only tool".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"],
                "additionalProperties": false
            }),
        }
    }
    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::ReadOnly
    }
    async fn execute(&self, _ctx: &ToolCtx, _args: Value) -> Result<ToolOutput> {
        self.ran.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutput::text("counted"))
    }
}

struct SchemaRecordingTool {
    calls: Arc<Mutex<Vec<Value>>>,
}

#[async_trait]
impl Tool for SchemaRecordingTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "schema_recorder".into(),
            description: "records schema-validated arguments".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {"query": {"type": "string"}},
                "required": ["query"],
                "additionalProperties": false,
                "x-fixture-constraint": {"mode": "advisory"}
            }),
        }
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::ReadOnly
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        self.calls.lock().unwrap().push(args);
        Ok(ToolOutput::text("recorded"))
    }
}

struct InvalidSchemaCountingTool {
    ran: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for InvalidSchemaCountingTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "invalid_server_schema".into(),
            description: "a tool whose advertised schema cannot compile".into(),
            input_schema: serde_json::json!({"type": "nonsense"}),
        }
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::ReadOnly
    }

    async fn execute(&self, _ctx: &ToolCtx, _args: Value) -> Result<ToolOutput> {
        self.ran.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutput::text("ran"))
    }
}

#[tokio::test]
async fn registry_dispatch_rejects_schema_mismatches_and_preserves_valid_arguments() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let registry = ToolRegistry::new().with(Box::new(SchemaRecordingTool {
        calls: calls.clone(),
    }));
    let tool = registry.get("schema_recorder").unwrap();
    let context = ToolCtx::new_legacy_workspace(
        SessionId::new(),
        None,
        std::path::PathBuf::from("unused-by-schema-recorder"),
    );

    let refused = tool
        .execute(&context, serde_json::json!({"query": 42}))
        .await
        .unwrap();
    assert!(refused.is_error);
    assert_eq!(
        refused.error_category,
        Some(ToolErrorCategory::InvalidArguments)
    );
    assert!(calls.lock().unwrap().is_empty());

    // The unrecognized extension is advisory: supported constraints still
    // apply, while a conforming call crosses the boundary byte-for-byte.
    let valid = serde_json::json!({"query": "waves"});
    let accepted = tool.execute(&context, valid.clone()).await.unwrap();
    assert!(!accepted.is_error);
    assert_eq!(*calls.lock().unwrap(), vec![valid]);
}

#[test]
fn registry_refuses_schema_mismatches_for_server_and_client_tools() {
    let spec = ToolSpec {
        name: "client_schema".into(),
        description: "a schema-validated client tool".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "labels": {
                    "type": "array",
                    "items": {"type": "string"},
                    "minContains": 1,
                    "contains": {"const": "required"}
                }
            },
            "required": ["labels"]
        }),
    };
    let mut registry = ToolRegistry::new().with(Box::new(StrictCountingTool {
        ran: Arc::new(AtomicUsize::new(0)),
    }));
    registry.register_client(spec, ApprovalClass::ReadOnly);

    assert!(registry
        .schema_mismatch("strict_counter", &serde_json::json!({"path": 42}))
        .is_some_and(|mismatch| mismatch.contains("string")));
    assert_eq!(
        registry.schema_mismatch(
            "client_schema",
            &serde_json::json!({"labels": ["other", "required"]})
        ),
        None
    );
    assert!(registry
        .schema_mismatch("client_schema", &serde_json::json!({"labels": ["other"]}))
        .is_some());
}

/// The narration argument is advertised as required so models reliably write
/// one, but it is display-only: a call that omits it must still run rather than
/// spend a round trip being corrected. A tool that genuinely requires its own
/// `summary` keeps its contract — the relaxation is keyed on our wording.
#[test]
fn a_missing_narration_does_not_refuse_a_call_but_a_foreign_summary_still_does() {
    let mut registry = ToolRegistry::new();
    registry.register_client(
        ToolSpec {
            name: "narrated".into(),
            description: "a tool carrying Tidebreak's narration argument".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "summary": {
                        "type": "string",
                        "description": crate::SUMMARY_ARGUMENT_DESCRIPTION
                    },
                    "query": {"type": "string"}
                },
                "required": ["summary", "query"]
            }),
        },
        ApprovalClass::ReadOnly,
    );
    registry.register_client(
        ToolSpec {
            name: "foreign".into(),
            description: "a mounted tool whose own summary is load-bearing".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "summary": {"type": "string", "description": "The text to file."}
                },
                "required": ["summary"]
            }),
        },
        ApprovalClass::ReadOnly,
    );

    assert_eq!(
        registry.schema_mismatch("narrated", &serde_json::json!({"query": "tides"})),
        None
    );
    // Relaxing `required` must not stop the field being checked when present.
    assert!(registry
        .schema_mismatch("narrated", &serde_json::json!({"query": "t", "summary": 7}))
        .is_some());
    assert!(registry
        .schema_mismatch("narrated", &serde_json::json!({}))
        .is_some_and(|mismatch| mismatch.contains("query")));
    assert!(registry
        .schema_mismatch("foreign", &serde_json::json!({}))
        .is_some());
}

#[tokio::test]
async fn registry_fails_closed_for_every_consumer_when_a_tool_schema_is_unusable() {
    let mut registry = ToolRegistry::new();
    registry.register_client(
        ToolSpec {
            name: "invalid_schema".into(),
            description: "a tool with a misconfigured schema".into(),
            input_schema: serde_json::json!({"type": "nonsense"}),
        },
        ApprovalClass::ReadOnly,
    );
    registry.register_client(
        ToolSpec {
            name: "unsupported_schema".into(),
            description: "a tool declaring an unsupported schema dialect".into(),
            input_schema: serde_json::json!({
                "$schema": "https://example.com/unknown-json-schema-dialect",
                "type": "object"
            }),
        },
        ApprovalClass::ReadOnly,
    );
    let ran = Arc::new(AtomicUsize::new(0));
    registry.register(Box::new(InvalidSchemaCountingTool { ran: ran.clone() }));

    assert!(registry
        .schema_mismatch("invalid_schema", &serde_json::json!({}))
        .is_some_and(|mismatch| mismatch.contains("could not be compiled")));
    assert!(!registry.client_arguments_are_valid("invalid_schema", &serde_json::json!({})));
    assert!(registry
        .schema_mismatch("unsupported_schema", &serde_json::json!({}))
        .is_some_and(|mismatch| mismatch.contains("unsupported")));
    assert!(!registry.client_arguments_are_valid("unsupported_schema", &serde_json::json!({})));
    let advertised = registry.specs_for_surface(true, false);
    assert!(!advertised.iter().any(|spec| spec.name == "invalid_schema"));
    assert!(!advertised
        .iter()
        .any(|spec| spec.name == "unsupported_schema"));
    assert!(!advertised
        .iter()
        .any(|spec| spec.name == "invalid_server_schema"));

    let context = ToolCtx::new_legacy_workspace(
        SessionId::new(),
        None,
        std::path::PathBuf::from("unused-by-invalid-schema-tool"),
    );
    let refused = registry
        .get("invalid_server_schema")
        .unwrap()
        .execute(&context, serde_json::json!({}))
        .await
        .unwrap();
    assert!(refused.is_error);
    assert_eq!(
        refused.error_category,
        Some(ToolErrorCategory::InvalidArguments)
    );
    assert_eq!(ran.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn arguments_violating_the_advertised_schema_are_refused_before_the_tool_runs() {
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
    let ran = Arc::new(AtomicUsize::new(0));

    let agent = Agent::new(
        Arc::new(SchemaArgsProvider {
            calls: AtomicUsize::new(0),
        }),
        Arc::new(ToolRegistry::new().with(Box::new(StrictCountingTool { ran: ran.clone() }))),
        store,
        AgentConfig {
            model: "fake".into(),
            ..Default::default()
        },
    );

    let (tx, rx) = unbounded();
    agent.run_turn(&chat, "count something", &tx).await.unwrap();
    drop(tx);
    let events: Vec<AgentEvent> = rx.collect().await;

    // Only the conforming call reached the tool.
    assert_eq!(ran.load(Ordering::SeqCst), 1, "exactly one call ran");
    let outputs: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolCallCompleted { output, .. } => Some(output),
            _ => None,
        })
        .collect();
    assert_eq!(outputs.len(), 2);
    let refused = outputs[0];
    assert!(refused.is_error);
    assert_eq!(
        refused.error_category,
        Some(ToolErrorCategory::InvalidArguments)
    );
    // The mismatch and the schema ride along so the model can re-emit.
    assert!(refused.content.contains("\"path\""), "{}", refused.content);
    assert!(!outputs[1].is_error);
}

/// A read-only tool that records whether it ran.
struct CountingTool {
    ran: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for CountingTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "counter".into(),
            description: "a read-only tool".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {"path": {"type": "string"}}
            }),
        }
    }
    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::ReadOnly
    }
    async fn execute(&self, _ctx: &ToolCtx, _args: Value) -> Result<ToolOutput> {
        self.ran.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutput::text("counted"))
    }
}

/// Streams a truncated argument fragment for `counter`, then finishes.
struct TruncatedArgsProvider {
    calls: AtomicUsize,
}

#[async_trait]
impl ModelProvider for TruncatedArgsProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("truncated")
    }
    async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        let events = if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            vec![
                ProviderEvent::ToolCallStarted {
                    index: 0,
                    id: "call_counter".into(),
                    name: "counter".into(),
                },
                ProviderEvent::ToolCallArgsDelta {
                    index: 0,
                    fragment: r#"{"path": "note"#.into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::ToolUse,
                },
            ]
        } else {
            vec![
                ProviderEvent::TextDelta { text: "ok".into() },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ]
        };
        Ok(stream::iter(events).boxed())
    }
}

#[tokio::test]
async fn malformed_arguments_go_back_to_the_model_instead_of_running_the_tool() {
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
    let ran = Arc::new(AtomicUsize::new(0));

    let agent = Agent::new(
        Arc::new(TruncatedArgsProvider {
            calls: AtomicUsize::new(0),
        }),
        Arc::new(ToolRegistry::new().with(Box::new(CountingTool { ran: ran.clone() }))),
        store.clone(),
        AgentConfig {
            model: "fake".into(),
            ..Default::default()
        },
    );

    let (tx, rx) = unbounded();
    agent.run_turn(&chat, "count something", &tx).await.unwrap();
    drop(tx);
    let events: Vec<AgentEvent> = rx.collect().await;

    assert_eq!(ran.load(Ordering::SeqCst), 0, "tool must not have run");
    let output = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::ToolCallCompleted { output, .. } => Some(output.clone()),
            _ => None,
        })
        .expect("the call was answered");
    assert!(output.is_error);
    assert_eq!(
        output.error_category,
        Some(ToolErrorCategory::InvalidArguments)
    );
    // The schema rides along so the model can re-emit the call.
    assert!(output.content.contains("\"path\""), "{}", output.content);

    // The garbled fragment survives to the journal: the durable record
    // shows what the provider actually streamed, not only the coerced
    // empty object a post-hoc debugging session cannot learn from.
    let recorded = store.list_tool_calls(chat.id).await.unwrap();
    let call = recorded
        .iter()
        .find(|call| call.name == "counter")
        .expect("the refused call was still recorded");
    assert_eq!(call.arguments, serde_json::json!({}));
    assert_eq!(call.raw_arguments.as_deref(), Some(r#"{"path": "note"#));
}
