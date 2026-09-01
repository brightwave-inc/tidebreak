use super::*;

struct RestartTool {
    ran: Arc<AtomicUsize>,
    class: ApprovalClass,
}

#[async_trait]
impl Tool for RestartTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "search".into(),
            description: "recover search".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }

    fn approval_class(&self) -> ApprovalClass {
        self.class
    }

    async fn execute(&self, _ctx: &ToolCtx, _args: Value) -> Result<ToolOutput> {
        self.ran.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutput::text("recovered result"))
    }
}

struct RestartGate(Arc<dyn Store>);

impl ApprovalGate for RestartGate {
    fn register(
        &self,
        request: ApprovalRequest,
        _journal: Option<crate::approval::ApprovalJournalIdentity>,
    ) -> crate::approval::ApprovalRegistrationFuture<'_> {
        let store = self.0.clone();
        Box::pin(async move {
            let approval = store
                .get_tool_call_approval(request.call_id)
                .await
                .unwrap()
                .expect("approval receipt must survive restart");
            let decision = match approval.decision() {
                Some(decision) => decision,
                None => {
                    store
                        .decide_tool_call_approval(
                            request.chat_id,
                            request.call_id,
                            &ApprovalDecision::Approve,
                            Utc::now(),
                        )
                        .await
                        .unwrap();
                    ApprovalDecision::Approve
                }
            };
            crate::approval::ApprovalRegistration {
                decision: Box::pin(async move { decision }),
                publication: crate::approval::ApprovalRequiredPublication::None,
            }
        })
    }
}

struct RestartProvider {
    provider_id: String,
    expect_error: bool,
}

#[async_trait]
impl ModelProvider for RestartProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("restart")
    }

    async fn stream(&self, request: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        assert!(request.messages.iter().any(|message| {
            message.content.iter().any(|block| {
                matches!(
                    block,
                ContentBlock::ToolResult { tool_use_id, is_error, .. }
                    if tool_use_id == &self.provider_id && *is_error == self.expect_error
                )
            })
        }));
        Ok(stream::iter(vec![
            ProviderEvent::TextDelta {
                text: "done".into(),
            },
            ProviderEvent::Stop {
                reason: StopReason::EndTurn,
            },
        ])
        .boxed())
    }
}

async fn assert_sensitive_restart_recovery(
    preapproved: bool,
    current_class: ApprovalClass,
    tool_present: bool,
) {
    let db = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            db.path().join("restart.db").display()
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
    let turn_id = TurnId::new();
    let accepted = match store
        .accept_turn(turn_id, chat.id, "fake", "search")
        .await
        .unwrap()
    {
        crate::storage::AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected turn acceptance: {outcome:?}"),
    };
    let lease_token = uuid::Uuid::new_v4();
    let claimed_at = Utc::now().max(accepted.available_at);
    store
        .claim_turn_run(
            lease_token,
            claimed_at,
            claimed_at + chrono::Duration::minutes(5),
        )
        .await
        .unwrap();
    let call = ToolCallRecord {
        id: CallId::new(),
        chat_id: chat.id,
        turn_id,
        provider_id: "persisted-search".into(),
        name: "search".into(),
        arguments: serde_json::json!({"query": "restart"}),
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
        created_at: claimed_at,
        resolved_at: None,
    };
    assert!(matches!(
        store
            .accept_claimed_tool_call(&call, lease_token, claimed_at)
            .await
            .unwrap(),
        AcceptClaimedToolCallOutcome::Accepted(_)
    ));
    store
        .request_tool_call_approval(
            &ApprovalRequest {
                auto_judge: false,
                call_id: call.id,
                chat_id: chat.id,
                turn_id,
                tool_name: call.name.clone(),
                class: ApprovalClass::Sensitive,
                kind: ToolApprovalKind::for_tool_name(&call.name),
                preview: None,
            },
            Utc::now(),
        )
        .await
        .unwrap();
    if preapproved {
        store
            .decide_tool_call_approval(chat.id, call.id, &ApprovalDecision::Approve, Utc::now())
            .await
            .unwrap();
    }
    let ran = Arc::new(AtomicUsize::new(0));
    let mut registry = ToolRegistry::new();
    if tool_present {
        registry.register(Box::new(RestartTool {
            ran: ran.clone(),
            class: current_class,
        }));
    }
    let agent = Agent::new(
        Arc::new(RestartProvider {
            provider_id: call.provider_id.clone(),
            expect_error: true,
        }),
        Arc::new(registry),
        store.clone(),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    )
    .with_approvals(Arc::new(RestartGate(store.clone())))
    .with_durable_steer(lease_token);
    let (tx, mut rx) = unbounded();
    let events = tokio::spawn(async move {
        let mut collected = Vec::new();
        while let Some(event) = rx.next().await {
            match event {
                ClaimedAgentEvent::Flush(acknowledge) => {
                    let _ = acknowledge.send(());
                }
                ClaimedAgentEvent::Pending { event, .. } => collected.push(event),
                ClaimedAgentEvent::Committed { event, .. }
                | ClaimedAgentEvent::Recovered { event, .. } => {
                    collected.push(event.event);
                }
            }
        }
        collected
    });
    assert!(matches!(
        agent
            .run_claimed_turn(&chat, turn_id, MessageId::new(), 1, &tx)
            .await
            .unwrap(),
        AgentTurnOutcome::Completed { .. }
    ));
    drop(tx);
    let events = events.await.unwrap();
    assert_eq!(ran.load(Ordering::SeqCst), 0);
    assert_eq!(
        store
            .list_tool_calls(chat.id)
            .await
            .unwrap()
            .into_iter()
            .find(|stored| stored.id == call.id)
            .unwrap()
            .status,
        ToolCallStatus::Failed
    );
    let approval_decided = events
        .iter()
        .position(|event| {
            matches!(
                event,
                AgentEvent::ApprovalDecided { call_id, .. } if *call_id == call.id
            )
        })
        .expect("recovery must close its durable approval card");
    let tool_completed = events
        .iter()
        .position(|event| {
            matches!(
                event,
                AgentEvent::ToolCallCompleted { call_id, .. } if *call_id == call.id
            )
        })
        .expect("recovery must publish its failed completion");
    assert!(approval_decided < tool_completed);
}

#[tokio::test]
async fn reclaimed_turn_suppresses_pending_and_preapproved_sensitive_calls() {
    assert_sensitive_restart_recovery(false, ApprovalClass::ReadOnly, true).await;
    assert_sensitive_restart_recovery(true, ApprovalClass::Sensitive, true).await;
    assert_sensitive_restart_recovery(false, ApprovalClass::ReadOnly, false).await;
}

async fn pending_workspace_restart(
    name: &str,
    arguments: Value,
) -> (
    tempfile::TempDir,
    Arc<dyn Store>,
    Chat,
    TurnId,
    uuid::Uuid,
    ToolCallRecord,
) {
    let db = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            db.path().join("cancelled-restart.db").display()
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
    let turn_id = TurnId::new();
    let accepted = match store
        .accept_turn(turn_id, chat.id, "fake", "recover workspace call")
        .await
        .unwrap()
    {
        crate::storage::AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected turn acceptance: {outcome:?}"),
    };
    let lease_token = uuid::Uuid::new_v4();
    let claimed_at = Utc::now().max(accepted.available_at);
    assert!(store
        .claim_turn_run(
            lease_token,
            claimed_at,
            claimed_at + chrono::Duration::minutes(5),
        )
        .await
        .unwrap()
        .turn
        .is_some());
    let call = ToolCallRecord {
        id: CallId::new(),
        chat_id: chat.id,
        turn_id,
        provider_id: "persisted-workspace-call".into(),
        name: name.into(),
        arguments,
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
        created_at: claimed_at,
        resolved_at: None,
    };
    assert!(matches!(
        store
            .accept_claimed_tool_call(&call, lease_token, claimed_at)
            .await
            .unwrap(),
        AcceptClaimedToolCallOutcome::Accepted(_)
    ));
    (db, store, chat, turn_id, lease_token, call)
}

#[tokio::test]
async fn cancelled_reclaim_resolves_pending_write_without_touching_scratch() {
    let scratch = tempfile::tempdir().unwrap();
    let (_db, store, chat, turn_id, lease_token, call) = pending_workspace_restart(
        "write_file",
        serde_json::json!({"path": "cancelled.txt", "content": "must not exist"}),
    )
    .await;
    store
        .request_tool_call_approval(
            &ApprovalRequest {
                auto_judge: false,
                call_id: call.id,
                chat_id: chat.id,
                turn_id,
                tool_name: call.name.clone(),
                class: ApprovalClass::Sensitive,
                kind: ToolApprovalKind::for_tool_name(&call.name),
                preview: None,
            },
            Utc::now(),
        )
        .await
        .unwrap();
    let cancel = CancelToken::new();
    cancel.cancel();
    let provider = Arc::new(BoomProvider {
        calls: AtomicUsize::new(0),
    });
    let agent = Agent::new(
        provider.clone(),
        Arc::new(ToolRegistry::new().with(Box::new(WriteFile))),
        store.clone(),
        AgentConfig {
            model: "fake".into(),
            tool_scratch: Some(tool_scratch(scratch.path())),
            ..AgentConfig::default()
        },
    )
    .with_cancel(cancel)
    .with_durable_steer(lease_token);
    let (tx, rx) = unbounded();
    assert!(matches!(
        agent
            .run_claimed_turn(&chat, turn_id, MessageId::new(), 1, &tx)
            .await
            .unwrap(),
        AgentTurnOutcome::Cancelled { .. }
    ));
    drop(tx);
    let events = emitted_events(rx.collect().await);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    assert!(!scratch.path().join("cancelled.txt").exists());
    let approval_decided = events
        .iter()
        .position(|event| {
            matches!(
                event,
                AgentEvent::ApprovalDecided {
                    call_id,
                    approved: false,
                } if *call_id == call.id
            )
        })
        .expect("cancelled recovery must close its durable approval card");
    let tool_completed = events
            .iter()
            .position(|event| {
                matches!(event, AgentEvent::ToolCallCompleted { call_id, .. } if *call_id == call.id)
            })
            .expect("cancelled recovery must publish failed tool completion");
    assert!(approval_decided < tool_completed);
    assert_eq!(
        store
            .list_tool_calls(chat.id)
            .await
            .unwrap()
            .into_iter()
            .find(|stored| stored.id == call.id)
            .unwrap()
            .status,
        ToolCallStatus::Failed
    );
}

struct CancelDuringRecoveryTool {
    cancel: CancelToken,
    classifications: AtomicUsize,
    ran: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for CancelDuringRecoveryTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "recovery_write".into(),
            description: "test recovery cancellation fence".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }

    fn approval_class(&self) -> ApprovalClass {
        if self.classifications.fetch_add(1, Ordering::SeqCst) == 1 {
            self.cancel.cancel();
        }
        ApprovalClass::Workspace
    }

    async fn execute(&self, _ctx: &ToolCtx, _args: Value) -> Result<ToolOutput> {
        self.ran.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutput::text("unexpected execution"))
    }
}

#[tokio::test]
async fn recovery_never_reexecutes_a_pending_workspace_call() {
    let (_db, store, chat, turn_id, lease_token, call) =
        pending_workspace_restart("recovery_write", serde_json::json!({})).await;
    let cancel = CancelToken::new();
    let ran = Arc::new(AtomicUsize::new(0));
    let tool = CancelDuringRecoveryTool {
        cancel: cancel.clone(),
        classifications: AtomicUsize::new(0),
        ran: ran.clone(),
    };
    let agent = Agent::new(
        Arc::new(BoomProvider {
            calls: AtomicUsize::new(0),
        }),
        Arc::new(ToolRegistry::new().with(Box::new(tool))),
        store.clone(),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    )
    .with_cancel(cancel)
    .with_durable_steer(lease_token);
    let (tx, _rx) = unbounded();
    assert!(matches!(
        agent
            .run_claimed_turn(&chat, turn_id, MessageId::new(), 1, &tx)
            .await
            .unwrap(),
        AgentTurnOutcome::Completed { .. }
    ));
    assert_eq!(ran.load(Ordering::SeqCst), 0);
    assert_eq!(
        store
            .list_tool_calls(chat.id)
            .await
            .unwrap()
            .into_iter()
            .find(|stored| stored.id == call.id)
            .unwrap()
            .status,
        ToolCallStatus::Failed
    );
}
