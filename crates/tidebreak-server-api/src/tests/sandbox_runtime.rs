use super::*;
use crate::bus::EventBus;
use crate::state::SandboxAttemptGuard;

fn sandbox_runtime_chat() -> Chat {
    Chat {
        id: SessionId::new(),
        project_id: None,
        title: Some("sandbox runtime".into()),
        model: Some("sandbox-test-model".into()),
        reasoning_effort: None,
        permission_mode: None,
        network_policy: Default::default(),
        attachment_revision: 0,
        root_attachments: Vec::new(),
        memory_incognito: false,
        created_at: chrono::Utc::now(),
    }
}

fn sandbox_runtime_worker(
    store: Arc<dyn Store>,
    provider: Arc<dyn ModelProvider>,
    agent_config: AgentConfig,
) -> sandbox_agent_run_worker::SandboxAgentRunWorker {
    let secrets: Arc<dyn SecretProvider> = Arc::new(MemSecrets::default());
    let resolver: Arc<dyn ProviderResolver> = Arc::new(FixedResolver(provider));
    let host = Arc::new(crate::sandbox_runtime::ServerSandboxHost::new(
        store.clone(),
        secrets,
        resolver,
        Arc::new(EventBus::default()),
        None,
    ));
    sandbox_agent_run_worker::SandboxAgentRunWorker::with_attempts(
        store,
        host,
        Arc::new(Notify::new()),
        Arc::new(Notify::new()),
        Arc::new(SandboxAttemptGuard::default()),
        agent_config,
        None,
        sandbox_agent_run_worker::SandboxAgentRunWorkerConfig::default(),
    )
}

struct SandboxTaskPlanThenDoneProvider {
    requests: Mutex<Vec<ChatRequest>>,
    plan_calls: usize,
}

impl Default for SandboxTaskPlanThenDoneProvider {
    fn default() -> Self {
        Self {
            requests: Mutex::new(Vec::new()),
            plan_calls: 2,
        }
    }
}

impl SandboxTaskPlanThenDoneProvider {
    fn with_plan_calls(plan_calls: usize) -> Self {
        Self {
            plan_calls,
            ..Self::default()
        }
    }

    fn plan_call(id: &str, arguments: &str) -> Vec<ProviderEvent> {
        vec![
            ProviderEvent::TextDelta {
                text: "Writing the plan down first.".into(),
            },
            ProviderEvent::ToolCallStarted {
                index: 0,
                id: id.into(),
                name: tidebreak_core::UPDATE_TASK_PLAN_TOOL.into(),
            },
            ProviderEvent::ToolCallArgsDelta {
                index: 0,
                fragment: arguments.into(),
            },
            ProviderEvent::Stop {
                reason: StopReason::ToolUse,
            },
        ]
    }

    fn done_call(id: &str) -> Vec<ProviderEvent> {
        vec![
            ProviderEvent::ToolCallStarted {
                index: 0,
                id: id.into(),
                name: tidebreak_core::SANDBOX_DONE_TOOL.into(),
            },
            ProviderEvent::ToolCallArgsDelta {
                index: 0,
                fragment: r#"{"outputs":[],"summary":"finished the research"}"#.into(),
            },
            ProviderEvent::Stop {
                reason: StopReason::ToolUse,
            },
        ]
    }
}

#[async_trait]
impl ModelProvider for SandboxTaskPlanThenDoneProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("sandbox-task-plan")
    }

    async fn stream(&self, request: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        let call_number = {
            let mut requests = self.requests.lock().unwrap();
            requests.push(request);
            requests.len()
        };
        let events = if call_number == 1 {
            Self::plan_call(
                "plan_1",
                r#"{"steps":[{"content":"read the brief","status":"in_progress"},{"content":"write the summary","status":"pending"}]}"#,
            )
        } else if call_number <= self.plan_calls {
            Self::plan_call(
                &format!("plan_{call_number}"),
                r#"{"steps":[{"content":"read the brief","status":"completed"},{"content":"write the summary","status":"in_progress"}]}"#,
            )
        } else {
            Self::done_call(&format!("done_{call_number}"))
        };
        Ok(stream::iter(events).boxed())
    }
}

#[tokio::test]
async fn sandbox_runtime_keeps_a_plan_and_reminds_once_before_completion() {
    let (_dir, store) = temp_db_store("sandbox-runtime-plan.db").await;
    let store: Arc<dyn Store> = Arc::new(store);
    let chat = sandbox_runtime_chat();
    store.create_chat(&chat).await.unwrap();
    let provider = Arc::new(SandboxTaskPlanThenDoneProvider::default());
    let worker = sandbox_runtime_worker(
        store.clone(),
        provider.clone(),
        AgentConfig {
            model: "sandbox-test-model".into(),
            max_steps: 8,
            ..AgentConfig::default()
        },
    );
    let plans = sandbox_task_plan_worker::SandboxTaskPlanWorker::new(
        store.clone(),
        Arc::new(Notify::new()),
        sandbox_task_plan_worker::SandboxTaskPlanWorkerConfig::default(),
    );
    let run = admit_sandbox_for_test(&store, chat.id, "Research this.").await;

    assert!(matches!(
        worker.run_once().await.unwrap(),
        sandbox_agent_run_worker::SandboxAgentRunWorkerOutcome::ToolCheckpointed(_)
    ));
    assert!(provider.requests.lock().unwrap()[0]
        .tools
        .iter()
        .any(|tool| tool.name == tidebreak_core::UPDATE_TASK_PLAN_TOOL));
    assert!(store
        .get_agent_run_task_plan(run.id)
        .await
        .unwrap()
        .is_none());
    assert!(matches!(
        plans.run_once().await.unwrap(),
        sandbox_task_plan_worker::SandboxTaskPlanWorkerOutcome::Resolved(_)
    ));
    let first = store
        .get_agent_run_task_plan(run.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.run_id, run.id);
    assert_eq!(
        first.steps[0].status,
        tidebreak_core::TaskPlanStepStatus::InProgress
    );

    assert!(matches!(
        worker.run_once().await.unwrap(),
        sandbox_agent_run_worker::SandboxAgentRunWorkerOutcome::ToolCheckpointed(_)
    ));
    assert!(matches!(
        plans.run_once().await.unwrap(),
        sandbox_task_plan_worker::SandboxTaskPlanWorkerOutcome::Resolved(_)
    ));
    let second = store
        .get_agent_run_task_plan(run.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(second.steps.len(), 2);
    assert_eq!(
        second.steps[0].status,
        tidebreak_core::TaskPlanStepStatus::Completed
    );
    assert_eq!(
        second.steps[1].status,
        tidebreak_core::TaskPlanStepStatus::InProgress
    );

    let reminder = match worker.run_once().await.unwrap() {
        sandbox_agent_run_worker::SandboxAgentRunWorkerOutcome::ToolCheckpointed(call_id) => {
            call_id
        }
        outcome => panic!("unexpected outcome: {outcome:?}"),
    };
    let receipt = store
        .get_sandbox_tool_call_receipt(reminder)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        receipt.status,
        tidebreak_core::SandboxToolCallStatus::Failed
    );
    assert_eq!(receipt.error_code.as_deref(), Some("task_plan_incomplete"));
    assert!(receipt.result.contains("write the summary"));
    assert_eq!(
        store.get_agent_run(run.id).await.unwrap().unwrap().status,
        AgentRunStatus::RetryWait
    );

    assert_eq!(
        worker.run_once().await.unwrap(),
        sandbox_agent_run_worker::SandboxAgentRunWorkerOutcome::Completed(run.id)
    );
}

#[tokio::test]
async fn sandbox_runtime_submits_at_its_last_step_without_a_plan_reminder() {
    let (_dir, store) = temp_db_store("sandbox-runtime-last-step.db").await;
    let store: Arc<dyn Store> = Arc::new(store);
    let chat = sandbox_runtime_chat();
    store.create_chat(&chat).await.unwrap();
    let provider = Arc::new(SandboxTaskPlanThenDoneProvider::with_plan_calls(1));
    let worker = sandbox_runtime_worker(
        store.clone(),
        provider,
        AgentConfig {
            model: "sandbox-test-model".into(),
            max_steps: 2,
            ..AgentConfig::default()
        },
    );
    let plans = sandbox_task_plan_worker::SandboxTaskPlanWorker::new(
        store.clone(),
        Arc::new(Notify::new()),
        sandbox_task_plan_worker::SandboxTaskPlanWorkerConfig::default(),
    );
    let run = admit_sandbox_for_test(&store, chat.id, "Research this.").await;

    assert!(matches!(
        worker.run_once().await.unwrap(),
        sandbox_agent_run_worker::SandboxAgentRunWorkerOutcome::ToolCheckpointed(_)
    ));
    assert!(matches!(
        plans.run_once().await.unwrap(),
        sandbox_task_plan_worker::SandboxTaskPlanWorkerOutcome::Resolved(_)
    ));
    assert!(!tidebreak_core::open_task_plan_steps(
        &store
            .get_agent_run_task_plan(run.id)
            .await
            .unwrap()
            .unwrap()
            .steps
    )
    .is_empty());
    assert_eq!(
        worker.run_once().await.unwrap(),
        sandbox_agent_run_worker::SandboxAgentRunWorkerOutcome::Completed(run.id)
    );
}

#[derive(Default)]
struct SandboxCheckInThenFinalProvider {
    calls: AtomicUsize,
}

#[async_trait]
impl ModelProvider for SandboxCheckInThenFinalProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("sandbox-check-in")
    }

    async fn stream(&self, _request: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let events = if call == 0 {
            vec![
                ProviderEvent::ToolCallStarted {
                    index: 0,
                    id: "search_1".into(),
                    name: tidebreak_core::SANDBOX_WEB_SEARCH_TOOL.into(),
                },
                ProviderEvent::ToolCallArgsDelta {
                    index: 0,
                    fragment: r#"{"query":"Tidebreak"}"#.into(),
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

async fn spend_sandbox_runtime_cadence(
    store: &Arc<dyn Store>,
    run_id: tidebreak_core::AgentRunId,
    chat_id: SessionId,
    steps: usize,
) {
    for step in 0..steps {
        let worker_lease = uuid::Uuid::new_v4();
        assert_eq!(
            store
                .claim_agent_run(worker_lease, chrono::Duration::minutes(5), 4, 2)
                .await
                .unwrap()
                .expect("run should remain claimable")
                .id,
            run_id
        );
        let call = SandboxToolCallRequest {
            id: CallId::new(),
            agent_run_id: run_id,
            chat_id,
            provider_id: format!("seeded_search_{step}"),
            name: tidebreak_core::SANDBOX_WEB_SEARCH_TOOL.into(),
            arguments: serde_json::json!({"query": format!("step {step}")}),
        };
        let call_id = call.id;
        store
            .park_agent_run_for_sandbox_tool_calls(
                run_id,
                worker_lease,
                &[tidebreak_core::SandboxToolCallParkEntry {
                    call,
                    resolution: None,
                }],
            )
            .await
            .unwrap();
        let executor_lease = uuid::Uuid::new_v4();
        store
            .claim_sandbox_tool_call(call_id, executor_lease, chrono::Duration::minutes(1))
            .await
            .unwrap();
        store
            .resolve_sandbox_tool_call(
                call_id,
                executor_lease,
                &ToolCallResolution::Completed {
                    result: "{\"results\":[]}".into(),
                },
            )
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn parent_tools_resume_and_cancel_an_extracted_sandbox_worker() {
    let (dir, store) = temp_db_store("sandbox-runtime-parent-tools.db").await;
    let store: Arc<dyn Store> = Arc::new(store);
    let chat = sandbox_runtime_chat();
    store.create_chat(&chat).await.unwrap();
    let provider = Arc::new(SandboxCheckInThenFinalProvider::default());
    let worker = sandbox_runtime_worker(
        store.clone(),
        provider,
        AgentConfig {
            model: "sandbox-test-model".into(),
            max_steps: 8,
            ..AgentConfig::default()
        },
    );
    let run = admit_sandbox_for_test(&store, chat.id, "Produce a document.").await;
    spend_sandbox_runtime_cadence(&store, run.id, chat.id, 7).await;
    assert_eq!(
        worker.run_once().await.unwrap(),
        sandbox_agent_run_worker::SandboxAgentRunWorkerOutcome::CheckedIn(run.id)
    );

    let context = ToolCtx::new_legacy_workspace(chat.id, None, dir.path().join("ws"));
    let resume = agent_control_tools::ResumeAgentTool::new(store.clone());
    let output = Tool::execute(
        &resume,
        &context,
        serde_json::json!({"agent_id": run.id, "guidance": "Wrap it up."}),
    )
    .await
    .unwrap();
    assert!(!output.is_error, "resume should succeed: {output:?}");
    let resumed = store.get_agent_run(run.id).await.unwrap().unwrap();
    assert_eq!(resumed.status, AgentRunStatus::RetryWait);
    assert_eq!(resumed.checkin_grants, 1);

    let again = Tool::execute(&resume, &context, serde_json::json!({"agent_id": run.id}))
        .await
        .unwrap();
    assert!(again.is_error);

    assert_eq!(
        worker.run_once().await.unwrap(),
        sandbox_agent_run_worker::SandboxAgentRunWorkerOutcome::Completed(run.id)
    );
    let cancel = agent_control_tools::CancelAgentTool::new(store.clone());
    let output = Tool::execute(
        &cancel,
        &context,
        serde_json::json!({"agent_id": run.id, "reason": "no longer needed"}),
    )
    .await
    .unwrap();
    assert!(
        !output.is_error,
        "cancelling a finished run should report success: {output:?}"
    );
}
