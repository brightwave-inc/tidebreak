use super::*;

#[tokio::test]
async fn sensitive_tool_parks_until_approved() {
    use crate::approval::AutoApproveGate;

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

    let ran = Arc::new(AtomicUsize::new(0));
    let tools = Arc::new(ToolRegistry::new().with(Box::new(BoomTool { ran: ran.clone() })));
    let agent = Agent::new(
        Arc::new(BoomProvider {
            calls: AtomicUsize::new(0),
        }),
        tools,
        store,
        AgentConfig {
            model: "fake".into(),
            ..Default::default()
        },
    )
    .with_approvals(Arc::new(AutoApproveGate));

    let (tx, rx) = unbounded();
    agent.run_turn(&chat, "go", &tx).await.unwrap();
    drop(tx);
    let events: Vec<AgentEvent> = rx.collect().await;

    assert!(events
        .iter()
        .any(|e| matches!(e, AgentEvent::ApprovalRequired { .. })));
    // The decision is the approval row's, journaled where the row settles
    // (decision 0048 step 5); the loop reports only that the tool ran.
    assert!(!events
        .iter()
        .any(|e| matches!(e, AgentEvent::ApprovalDecided { .. })));
    assert_eq!(ran.load(Ordering::SeqCst), 1);
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::ToolCallCompleted { output, .. }
            if output.content == "boomed" && !output.is_error
    )));
}

/// Provider that prefaces a sensitive `boom` call with prose, then
/// finishes on the next step.
struct ProseBoomProvider {
    calls: AtomicUsize,
}

#[async_trait]
impl ModelProvider for ProseBoomProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("prose-boom")
    }
    async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        let events = if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            vec![
                ProviderEvent::TextDelta {
                    text: "I'll run the sensitive tool for you.".into(),
                },
                ProviderEvent::ToolCallStarted {
                    index: 0,
                    id: "call_boom".into(),
                    name: "boom".into(),
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
                ProviderEvent::TextDelta { text: "ok".into() },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ]
        };
        Ok(stream::iter(events).boxed())
    }
}

/// The failure that motivated #372: prose plus one sensitive call must
/// keep the preamble, persist it like any other text+tool step, and reach
/// the approval gate on the first step instead of burning the budget on
/// corrective retries.
#[tokio::test]
async fn sensitive_call_with_prose_keeps_the_preamble_and_parks() {
    use crate::approval::AutoApproveGate;

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

    let ran = Arc::new(AtomicUsize::new(0));
    let tools = Arc::new(ToolRegistry::new().with(Box::new(BoomTool { ran: ran.clone() })));
    let provider = Arc::new(ProseBoomProvider {
        calls: AtomicUsize::new(0),
    });
    let agent = Agent::new(
        provider.clone(),
        tools,
        store.clone(),
        AgentConfig {
            model: "fake".into(),
            ..Default::default()
        },
    )
    .with_approvals(Arc::new(AutoApproveGate));

    let (tx, rx) = unbounded();
    agent.run_turn(&chat, "go", &tx).await.unwrap();
    drop(tx);
    let events: Vec<AgentEvent> = rx.collect().await;

    // The step is never rejected or scrubbed: the streamed preamble stands.
    assert!(!events
        .iter()
        .any(|e| matches!(e, AgentEvent::StreamInterrupted)));
    // The call parks on the first step and runs once approved.
    assert!(events
        .iter()
        .any(|e| matches!(e, AgentEvent::ApprovalRequired { .. })));
    assert_eq!(ran.load(Ordering::SeqCst), 1);
    // No corrective retry: the tool step plus the closing step.
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    // The preamble is persisted exactly once, like any other text+tool step.
    let history = store.list_messages(chat.id).await.unwrap();
    assert_eq!(
        history
            .iter()
            .filter(|message| message.content.contains("sensitive tool for you"))
            .count(),
        1
    );
}

/// Provider that asks for two sensitive calls in one step. Both run, one
/// at a time — a parked call has to be the turn's only pending row, so the
/// second is admitted only once the first is terminal, never declined.
struct SiblingBoomProvider {
    calls: AtomicUsize,
}

#[async_trait]
impl ModelProvider for SiblingBoomProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("sibling-boom")
    }
    async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        let events = if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            vec![
                ProviderEvent::ToolCallStarted {
                    index: 0,
                    id: "call_a".into(),
                    name: "boom".into(),
                },
                ProviderEvent::ToolCallArgsDelta {
                    index: 0,
                    fragment: "{}".into(),
                },
                ProviderEvent::ToolCallStarted {
                    index: 1,
                    id: "call_b".into(),
                    name: "boom".into(),
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
async fn a_second_sensitive_call_runs_once_the_first_is_terminal() {
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

    let ran = Arc::new(AtomicUsize::new(0));
    let observed = Arc::new(Mutex::new(Vec::new()));
    let tools = Arc::new(ToolRegistry::new().with(Box::new(BoomTool { ran: ran.clone() })));
    let provider = Arc::new(SiblingBoomProvider {
        calls: AtomicUsize::new(0),
    });
    let agent = Agent::new(
        provider.clone(),
        tools,
        store.clone(),
        AgentConfig {
            model: "fake".into(),
            ..Default::default()
        },
    )
    .with_approvals(Arc::new(RecordingGate {
        store: store.clone(),
        chat_id: chat.id,
        observed: observed.clone(),
    }));

    let (tx, rx) = unbounded();
    agent.run_turn(&chat, "go", &tx).await.unwrap();
    drop(tx);
    let events: Vec<AgentEvent> = rx.collect().await;

    // The step stands and nothing is declined: each call parks in turn and
    // runs. A sibling used to be answered with "has to run on its own",
    // which forced the model to re-ask a step later for work it had
    // already requested correctly.
    assert!(!events
        .iter()
        .any(|e| matches!(e, AgentEvent::StreamInterrupted)));
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AgentEvent::ApprovalRequired { .. }))
            .count(),
        2
    );
    assert_eq!(ran.load(Ordering::SeqCst), 2);
    let completions: Vec<&ToolOutput> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolCallCompleted { output, .. } => Some(output),
            _ => None,
        })
        .collect();
    assert_eq!(completions.len(), 2, "{completions:?}");
    assert!(completions
        .iter()
        .all(|output| output.content == "boomed" && !output.is_error));
    // Both ran, so both leave a durable record.
    assert_eq!(store.list_tool_calls(chat.id).await.unwrap().len(), 2);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    // The recovery invariant, held at both parks: every earlier sibling is
    // terminal and the parked call is the turn's only pending row.
    let snapshots = observed.lock().unwrap().clone();
    assert_eq!(
        snapshots,
        vec![
            vec![("boom".into(), ToolCallStatus::Pending)],
            vec![
                ("boom".into(), ToolCallStatus::Completed),
                ("boom".into(), ToolCallStatus::Pending),
            ],
        ]
    );
}

/// Provider that pairs a plain server call with a sensitive one in the same
/// step, then finishes.
struct MixedBoomProvider {
    calls: AtomicUsize,
}

#[async_trait]
impl ModelProvider for MixedBoomProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("mixed-boom")
    }
    async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        let events = if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            vec![
                ProviderEvent::ToolCallStarted {
                    index: 0,
                    id: "call_read".into(),
                    name: "read_file".into(),
                },
                ProviderEvent::ToolCallArgsDelta {
                    index: 0,
                    fragment: r#"{"path":"a.txt"}"#.into(),
                },
                ProviderEvent::ToolCallStarted {
                    index: 1,
                    id: "call_boom".into(),
                    name: "boom".into(),
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
                ProviderEvent::TextDelta { text: "ok".into() },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ]
        };
        Ok(stream::iter(events).boxed())
    }
}

/// One durable-record snapshot per approval registration: each row's tool
/// name and status at the instant the gate saw the request.
type GateSnapshots = Arc<Mutex<Vec<Vec<(String, ToolCallStatus)>>>>;

/// Approval gate that photographs the durable record at the instant each
/// request is registered, then approves.
struct RecordingGate {
    store: Arc<dyn Store>,
    chat_id: ChatId,
    observed: GateSnapshots,
}

impl crate::approval::ApprovalGate for RecordingGate {
    fn register(
        &self,
        _request: crate::approval::ApprovalRequest,
        _journal: Option<crate::approval::ApprovalJournalIdentity>,
    ) -> crate::approval::ApprovalRegistrationFuture<'_> {
        Box::pin(async move {
            let calls = self.store.list_tool_calls(self.chat_id).await.unwrap();
            self.observed.lock().unwrap().push(
                calls
                    .into_iter()
                    .map(|call| (call.name, call.status))
                    .collect(),
            );
            crate::approval::ApprovalRegistration {
                decision: Box::pin(async { crate::approval::ApprovalDecision::Approve })
                    as crate::approval::ApprovalFuture,
                publication: crate::approval::ApprovalRequiredPublication::Ordinary,
            }
        })
    }
}

/// The resume invariant, stated as behaviour: a call that parks on the gate
/// is the turn's only pending row. The loop no longer refuses the batch to
/// get that — it admits the sensitive call after its plain siblings have
/// resolved, so `resume_pending_server_calls` has nothing to disambiguate.
#[tokio::test]
async fn a_sensitive_call_parks_only_after_its_plain_sibling_is_terminal() {
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("a.txt"), "read first").unwrap();
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

    let ran = Arc::new(AtomicUsize::new(0));
    let observed = Arc::new(Mutex::new(Vec::new()));
    let provider = Arc::new(MixedBoomProvider {
        calls: AtomicUsize::new(0),
    });
    let agent = Agent::new(
        provider.clone(),
        Arc::new(
            ToolRegistry::new()
                .with(Box::new(ReadFile))
                .with(Box::new(BoomTool { ran: ran.clone() })),
        ),
        store.clone(),
        AgentConfig {
            model: "fake".into(),
            tool_scratch: Some(tool_scratch(workspace.path())),
            ..Default::default()
        },
    )
    .with_approvals(Arc::new(RecordingGate {
        store: store.clone(),
        chat_id: chat.id,
        observed: observed.clone(),
    }));

    let (tx, rx) = unbounded();
    agent.run_turn(&chat, "go", &tx).await.unwrap();
    drop(tx);
    let events: Vec<AgentEvent> = rx.collect().await;

    assert!(!events
        .iter()
        .any(|e| matches!(e, AgentEvent::StreamInterrupted)));
    assert_eq!(ran.load(Ordering::SeqCst), 1);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    let at_approval = observed.lock().unwrap().last().cloned().unwrap();
    assert_eq!(
        at_approval,
        vec![
            ("read_file".into(), ToolCallStatus::Completed),
            ("boom".into(), ToolCallStatus::Pending),
        ],
        "the parked call must be the only pending row"
    );
}

/// A Sensitive, standing-grantable tool (`search`) that records whether it
/// ran.
struct SearchTool {
    ran: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for SearchTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "search".into(),
            description: "a sensitive search tool".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }
    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::Sensitive
    }
    async fn execute(&self, _ctx: &ToolCtx, _args: Value) -> Result<ToolOutput> {
        self.ran.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutput::text("searched"))
    }
}

/// Provider that asks for the `search` tool once, then finishes.
struct SearchProvider {
    calls: AtomicUsize,
}

#[async_trait]
impl ModelProvider for SearchProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("search")
    }
    async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        let events = if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            vec![
                ProviderEvent::ToolCallStarted {
                    index: 0,
                    id: "call_search".into(),
                    name: "search".into(),
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
                ProviderEvent::TextDelta { text: "ok".into() },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ]
        };
        Ok(stream::iter(events).boxed())
    }
}

fn search_agent(
    store: Arc<dyn Store>,
    ran: Arc<AtomicUsize>,
    grants: Arc<crate::approval::StandingGrants>,
) -> Agent {
    let tools = Arc::new(ToolRegistry::new().with(Box::new(SearchTool { ran })));
    // Default gate is `RefuseGate`: it rejects any call that reaches it, so
    // the tool running proves the standing grant bypassed the gate entirely.
    Agent::new(
        Arc::new(SearchProvider {
            calls: AtomicUsize::new(0),
        }),
        tools,
        store,
        AgentConfig {
            model: "fake".into(),
            ..Default::default()
        },
    )
    .with_standing_grants(grants)
}

#[tokio::test]
async fn standing_grant_runs_sensitive_tool_without_parking() {
    use crate::approval::{GrantLevel, StandingGrant, StandingGrants};

    let store = search_grant_store().await;
    let chat = search_grant_chat(&store).await;
    let grants = Arc::new(StandingGrants::from_grants(vec![StandingGrant::new(
        GrantLevel::Chat { chat_id: chat.id },
        "search",
        ToolApprovalKind::for_tool_name("search"),
        Utc::now(),
    )
    .expect("search is grantable")]));

    let ran = Arc::new(AtomicUsize::new(0));
    let agent = search_agent(store, ran.clone(), grants);

    let (tx, rx) = unbounded();
    agent.run_turn(&chat, "go", &tx).await.unwrap();
    drop(tx);
    let events: Vec<AgentEvent> = rx.collect().await;

    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::ApprovalRequired { .. })),
        "a covered call must not re-prompt"
    );
    assert_eq!(ran.load(Ordering::SeqCst), 1);
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::ToolCallCompleted { output, .. }
            if output.content == "searched" && !output.is_error
    )));
}

#[tokio::test]
async fn standing_grant_for_another_chat_does_not_bypass_the_gate() {
    use crate::approval::{GrantLevel, StandingGrant, StandingGrants};

    let store = search_grant_store().await;
    let chat = search_grant_chat(&store).await;
    // A grant scoped to a different chat must not cover this chat's call.
    let grants = Arc::new(StandingGrants::from_grants(vec![StandingGrant::new(
        GrantLevel::Chat {
            chat_id: ChatId::new(),
        },
        "search",
        ToolApprovalKind::for_tool_name("search"),
        Utc::now(),
    )
    .expect("search is grantable")]));

    let ran = Arc::new(AtomicUsize::new(0));
    let agent = search_agent(store, ran.clone(), grants);

    let (tx, rx) = unbounded();
    agent.run_turn(&chat, "go", &tx).await.unwrap();
    drop(tx);
    let events: Vec<AgentEvent> = rx.collect().await;

    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::ApprovalRequired { .. })),
        "an uncovered call must still park on the gate"
    );
    assert_eq!(ran.load(Ordering::SeqCst), 0, "RefuseGate blocks the tool");
}

async fn permission_mode_chat(store: &Arc<dyn Store>, mode: Option<crate::PermissionMode>) -> Chat {
    let chat = Chat {
        id: ChatId::new(),
        project_id: None,
        title: None,
        model: None,
        reasoning_effort: None,
        permission_mode: mode,
        network_policy: Default::default(),
        attachment_revision: 0,
        root_attachments: Vec::new(),
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();
    chat
}

/// A Workspace-class tool that records whether it ran.
struct WorkspaceWriteTool {
    ran: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for WorkspaceWriteTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "write_file".into(),
            description: "a workspace write tool".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }
    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::Workspace
    }
    async fn execute(&self, _ctx: &ToolCtx, _args: Value) -> Result<ToolOutput> {
        self.ran.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutput::text("written"))
    }
}

/// Provider that asks for `write_file` once, then finishes.
struct WorkspaceWriteProvider {
    calls: AtomicUsize,
}

#[async_trait]
impl ModelProvider for WorkspaceWriteProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("workspace-write")
    }
    async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        let events = if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            vec![
                ProviderEvent::ToolCallStarted {
                    index: 0,
                    id: "call_write".into(),
                    name: "write_file".into(),
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
                ProviderEvent::TextDelta { text: "ok".into() },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ]
        };
        Ok(stream::iter(events).boxed())
    }
}

fn workspace_write_agent(store: Arc<dyn Store>, ran: Arc<AtomicUsize>) -> Agent {
    let tools = Arc::new(ToolRegistry::new().with(Box::new(WorkspaceWriteTool { ran })));
    // Default gate is `RefuseGate`, so whether the tool runs is exactly
    // whether the mode kept the call off the gate.
    Agent::new(
        Arc::new(WorkspaceWriteProvider {
            calls: AtomicUsize::new(0),
        }),
        tools,
        store,
        AgentConfig {
            model: "fake".into(),
            ..Default::default()
        },
    )
}

/// The default mode is Ask, and Ask parks Workspace-class calls: reversing
/// either half of that silently stops asking before file edits.
#[tokio::test]
async fn ask_mode_parks_workspace_writes_by_default() {
    let store = search_grant_store().await;
    let chat = permission_mode_chat(&store, None).await;

    let ran = Arc::new(AtomicUsize::new(0));
    let agent = workspace_write_agent(store, ran.clone());

    let (tx, rx) = unbounded();
    agent.run_turn(&chat, "go", &tx).await.unwrap();
    drop(tx);
    let events: Vec<AgentEvent> = rx.collect().await;

    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::ApprovalRequired { class, kind, .. }
                if *class == ApprovalClass::Workspace
                    && *kind == ToolApprovalKind::WorkspaceMayModifyFiles
        )),
        "an uncovered workspace call must park in Ask"
    );
    assert_eq!(ran.load(Ordering::SeqCst), 0, "RefuseGate blocks the tool");
}

/// Auto keeps today's behavior: workspace writes proceed without a card.
#[tokio::test]
async fn auto_mode_runs_workspace_writes_without_asking() {
    let store = search_grant_store().await;
    let chat = permission_mode_chat(&store, Some(crate::PermissionMode::Auto)).await;

    let ran = Arc::new(AtomicUsize::new(0));
    let agent = workspace_write_agent(store, ran.clone());

    let (tx, rx) = unbounded();
    agent.run_turn(&chat, "go", &tx).await.unwrap();
    drop(tx);
    let events: Vec<AgentEvent> = rx.collect().await;

    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::ApprovalRequired { .. })),
        "Auto must not ask before a workspace write"
    );
    assert_eq!(ran.load(Ordering::SeqCst), 1);
}

/// Allow bypasses the gate for Sensitive calls entirely — no card, no
/// approval row, the tool just runs. The inverse regression (Allow still
/// parking) would make the mode a lie in the other direction.
#[tokio::test]
async fn allow_mode_runs_sensitive_without_the_gate() {
    let store = search_grant_store().await;
    let chat = permission_mode_chat(&store, Some(crate::PermissionMode::Allow)).await;

    let ran = Arc::new(AtomicUsize::new(0));
    let agent = search_agent(
        store,
        ran.clone(),
        Arc::new(crate::approval::StandingGrants::new()),
    );

    let (tx, rx) = unbounded();
    agent.run_turn(&chat, "go", &tx).await.unwrap();
    drop(tx);
    let events: Vec<AgentEvent> = rx.collect().await;

    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::ApprovalRequired { .. })),
        "Allow must not park a sensitive call"
    );
    assert_eq!(ran.load(Ordering::SeqCst), 1);
}

/// Plan mode refuses a mutating call outright: no approval card the
/// reader could accept, no tool run. Losing either half turns "plan mode
/// is read-only" into a prompt-level suggestion.
#[tokio::test]
async fn plan_mode_refuses_workspace_writes_without_parking() {
    let store = search_grant_store().await;
    let chat = permission_mode_chat(&store, Some(crate::PermissionMode::Plan)).await;

    let ran = Arc::new(AtomicUsize::new(0));
    let agent = workspace_write_agent(store, ran.clone());

    let (tx, rx) = unbounded();
    agent.run_turn(&chat, "go", &tx).await.unwrap();
    drop(tx);
    let events: Vec<AgentEvent> = rx.collect().await;

    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::ApprovalRequired { .. })),
        "plan mode must refuse, not park: there is nothing to approve"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::ToolCallCompleted { output, .. }
                if output.is_error && output.content.contains("plan mode")
        )),
        "the model must be told the call was refused because of plan mode"
    );
    assert_eq!(ran.load(Ordering::SeqCst), 0);
}

/// A standing grant made in another mode must not let a plan turn run a
/// mutating call: the refusal comes before grant matching on purpose.
#[tokio::test]
async fn plan_mode_standing_grant_does_not_bypass_the_refusal() {
    use crate::approval::{GrantLevel, StandingGrant, StandingGrants};

    let store = search_grant_store().await;
    let chat = permission_mode_chat(&store, Some(crate::PermissionMode::Plan)).await;
    let grants = Arc::new(StandingGrants::from_grants(vec![StandingGrant::new(
        GrantLevel::Chat { chat_id: chat.id },
        "search",
        ToolApprovalKind::for_tool_name("search"),
        Utc::now(),
    )
    .expect("search is grantable")]));

    let ran = Arc::new(AtomicUsize::new(0));
    let agent = search_agent(store, ran.clone(), grants);

    let (tx, rx) = unbounded();
    agent.run_turn(&chat, "go", &tx).await.unwrap();
    drop(tx);
    let events: Vec<AgentEvent> = rx.collect().await;

    assert_eq!(
        ran.load(Ordering::SeqCst),
        0,
        "a covered sensitive call must still be refused in plan mode"
    );
    assert!(!events
        .iter()
        .any(|e| matches!(e, AgentEvent::ApprovalRequired { .. })));
}

/// A plan-mode turn that would ask a question must not park: a headless
/// `--permission-mode plan` driver can decide a plan, not answer a card.
/// The surface withholds the tool, so this provider emits it anyway — the
/// same slip the live model made after listing folders and missing a file.
struct PlanModeQuestionProvider;

#[async_trait]
impl ModelProvider for PlanModeQuestionProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("plan-mode-question")
    }

    async fn stream(&self, req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        if req.tool_choice == Some(ToolChoice::None) {
            return Ok(stream::iter(vec![
                ProviderEvent::TextDelta {
                    text: "I'll list the missing file as a first step.".into(),
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
                id: "question_1".into(),
                name: crate::ASK_USER_QUESTIONS_TOOL.into(),
            },
            ProviderEvent::ToolCallArgsDelta {
                index: 0,
                fragment: r#"{"questions":[{"id":"missing","header":"File","question":"Where is buggy_sort.py?","options":[{"id":"scratch","label":"Scratch","description":"Look in private scratch."}],"allow_free_form":true}]}"#.into(),
            },
            ProviderEvent::Stop {
                reason: StopReason::ToolUse,
            },
        ])
        .boxed())
    }
}

#[tokio::test]
async fn plan_mode_does_not_run_ask_user_questions() {
    let db = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            db.path().join("plan-questions.db").display()
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
        permission_mode: Some(PermissionMode::Plan),
        network_policy: Default::default(),
        attachment_revision: 0,
        root_attachments: Vec::new(),
        created_at: Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    store
        .accept_turn(turn_id, chat.id, "fake", "find the missing file")
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
        crate::ask_user_questions_tool_spec(),
        ApprovalClass::ReadOnly,
        crate::validate_ask_user_questions_arguments,
    );
    let agent = Agent::new(
        Arc::new(PlanModeQuestionProvider),
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
    let events = emitted_events(rx.collect().await);

    assert!(
        !matches!(outcome, AgentTurnOutcome::ClientToolCall { .. }),
        "plan mode must refuse, not park: {outcome:?}"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::UserQuestionsAsked { .. })),
        "a declined question must not emit a parked card"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::ToolCallCompleted { output, .. }
                if output.is_error
                    && output.content.contains("plan mode")
                    && output.content.contains("exit_plan_mode")
        )),
        "the model must be told to put the missing input in the plan: {events:?}"
    );
    assert!(
        store.list_tool_calls(chat.id).await.unwrap().is_empty(),
        "a declined question must not leave a durable continuation"
    );
}

/// The plan surface advertises only read-only registrations, so the model
/// is never offered a tool the turn would refuse.
#[test]
fn plan_surface_advertises_only_read_only_tools() {
    let mut tools = ToolRegistry::new()
        .with(Box::new(ReadFile))
        .with(Box::new(WorkspaceWriteTool {
            ran: Arc::new(AtomicUsize::new(0)),
        }))
        .with(Box::new(SearchTool {
            ran: Arc::new(AtomicUsize::new(0)),
        }));
    tools.register_validated_client(
        crate::read_connected_file_tool_spec(),
        ApprovalClass::ReadOnly,
        crate::validate_read_connected_file_arguments,
    );
    tools.register_validated_client(
        crate::write_output_to_connected_folder_tool_spec(),
        ApprovalClass::Workspace,
        crate::validate_write_output_to_connected_folder_arguments,
    );
    tools.register_validated_foreground_client(
        crate::ask_user_questions_tool_spec(),
        ApprovalClass::ReadOnly,
        crate::validate_ask_user_questions_arguments,
    );
    tools.register_foreground_agent_orchestration();

    tools = tools.with(Box::new(TaskPlanStub));

    let mut names = tools
        .specs_for_surface(true, true)
        .into_iter()
        .map(|spec| spec.name)
        .collect::<Vec<_>>();
    names.sort();
    // `update_task_plan` is read-only by consent class but still commits a
    // durable row, and a plan-mode turn is drafting a proposal the reader has
    // not accepted. `ask_user_questions` is the same shape: read-only, but it
    // parks a continuation a plan driver cannot answer. Both are carved out
    // by name rather than by class.
    assert_eq!(names, vec!["read_connected_file", "read_file"]);
    assert!(tools
        .specs_for_surface(true, false)
        .iter()
        .any(|spec| spec.name == crate::UPDATE_TASK_PLAN_TOOL));
    assert!(tools
        .specs_for_surface(true, false)
        .iter()
        .any(|spec| spec.name == crate::ASK_USER_QUESTIONS_TOOL));
}

/// Stands in for the server-side task-plan tool: read-only class, real write.
struct TaskPlanStub;

#[async_trait]
impl Tool for TaskPlanStub {
    fn spec(&self) -> ToolSpec {
        crate::update_task_plan_tool_spec()
    }
    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::ReadOnly
    }
    async fn execute(&self, _ctx: &ToolCtx, _args: Value) -> Result<ToolOutput> {
        Ok(ToolOutput::text("recorded"))
    }
}

/// A Sensitive tool that escapes the chat workspace (`exec`) and records
/// whether it ran.
struct ExecTool {
    ran: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for ExecTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "exec".into(),
            description: "an escaping command execution tool".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }
    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::Sensitive
    }
    async fn execute(&self, _ctx: &ToolCtx, _args: Value) -> Result<ToolOutput> {
        self.ran.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutput::text("executed"))
    }
}

/// Provider that asks for the `exec` tool once, then finishes.
struct ExecProvider {
    calls: AtomicUsize,
}

#[async_trait]
impl ModelProvider for ExecProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("exec")
    }
    async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        let events = if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            vec![
                ProviderEvent::ToolCallStarted {
                    index: 0,
                    id: "call_exec".into(),
                    name: "exec".into(),
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
                ProviderEvent::TextDelta { text: "ok".into() },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ]
        };
        Ok(stream::iter(events).boxed())
    }
}

fn exec_agent(store: Arc<dyn Store>, ran: Arc<AtomicUsize>, grants: Arc<StandingGrants>) -> Agent {
    let tools = Arc::new(ToolRegistry::new().with(Box::new(ExecTool { ran })));
    // Default gate is `RefuseGate`: it rejects any call that reaches it, so
    // the tool running proves the standing grant bypassed the gate entirely.
    Agent::new(
        Arc::new(ExecProvider {
            calls: AtomicUsize::new(0),
        }),
        tools,
        store,
        AgentConfig {
            model: "fake".into(),
            ..Default::default()
        },
    )
    .with_standing_grants(grants)
}

#[tokio::test]
async fn standing_grant_runs_escaping_exec_without_parking() {
    use crate::approval::{GrantLevel, StandingGrant, StandingGrants};

    let store = search_grant_store().await;
    let chat = search_grant_chat(&store).await;
    let grants = Arc::new(StandingGrants::from_grants(vec![StandingGrant::new(
        GrantLevel::Chat { chat_id: chat.id },
        "exec",
        ToolApprovalKind::for_tool_name("exec"),
        Utc::now(),
    )
    .expect("exec is grantable")]));

    let ran = Arc::new(AtomicUsize::new(0));
    let agent = exec_agent(store, ran.clone(), grants);

    let (tx, rx) = unbounded();
    agent.run_turn(&chat, "go", &tx).await.unwrap();
    drop(tx);
    let events: Vec<AgentEvent> = rx.collect().await;

    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::ApprovalRequired { .. })),
        "a covered escaping call must not re-prompt"
    );
    assert_eq!(ran.load(Ordering::SeqCst), 1);
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::ToolCallCompleted { output, .. }
            if output.content == "executed" && !output.is_error
    )));
}

#[tokio::test]
async fn ungranted_escaping_exec_still_parks_deny_by_default() {
    use crate::approval::StandingGrants;

    let store = search_grant_store().await;
    let chat = search_grant_chat(&store).await;
    // No grant covers this chat: an escaping action must still park.
    let grants = Arc::new(StandingGrants::new());

    let ran = Arc::new(AtomicUsize::new(0));
    let agent = exec_agent(store, ran.clone(), grants);

    let (tx, rx) = unbounded();
    agent.run_turn(&chat, "go", &tx).await.unwrap();
    drop(tx);
    let events: Vec<AgentEvent> = rx.collect().await;

    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::ApprovalRequired { kind, .. }
                if *kind == ToolApprovalKind::ExecMayRunNetworkedCommand
        )),
        "an uncovered escaping call must park on the gate with a presentable kind"
    );
    assert_eq!(ran.load(Ordering::SeqCst), 0, "RefuseGate blocks the tool");
}

#[tokio::test]
async fn sensitive_tool_is_refused_without_a_gate() {
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

    let ran = Arc::new(AtomicUsize::new(0));
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
    );

    let (tx, rx) = unbounded();
    agent.run_turn(&chat, "go", &tx).await.unwrap();
    drop(tx);
    let events: Vec<AgentEvent> = rx.collect().await;

    assert_eq!(ran.load(Ordering::SeqCst), 0);
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::ToolCallCompleted { output, .. } if output.is_error
    )));
}
