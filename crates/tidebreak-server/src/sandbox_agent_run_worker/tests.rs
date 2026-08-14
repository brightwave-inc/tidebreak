#[cfg(test)]
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use tidebreak_core::{
    sandbox_done_tool_spec, sandbox_exec_tool_spec, sandbox_read_delegated_file_tool_spec,
    AgentConfig, AgentError, AgentRunStatus, CallId, ChatMessage, ChatRequest, ContentBlock,
    ModelProvider, ProviderEvent, Result, Role, SandboxToolCallStatus, SecretProvider, StopReason,
    Store, ToolCallRecord, ToolCallResolution, TurnWebSearch, SANDBOX_EXEC_TOOL,
};
use tokio::sync::Notify;

use crate::bus::EventBus;
use crate::resolver::ProviderResolver;
use crate::retry::RetrySchedule;
use crate::state::SandboxAttemptGuard;

use super::*;

use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::Mutex;
use std::task::{Context, Poll};

use async_trait::async_trait;
use chrono::Utc;
use futures::stream::{self, BoxStream};
use tidebreak_core::{
    Chat, ChatId, DbStore, ProviderId, ReasoningEffort, TurnCheckpointProgress, TurnId,
    TurnRunStatus, Usage,
};

use super::config::{
    sandbox_skills_summary, sandbox_system_prompt, SandboxAgentRunWorkerOutcome,
    SANDBOX_PROMPT_EXEC_CLAUSE, SANDBOX_PROMPT_SKILLS_INTRO, SANDBOX_PROMPT_WEB_SEARCH_CLAUSE,
};
use super::model_step::{
    complete_sandbox_task, delegated_file_admission_matches, sandbox_request, SandboxCompletion,
    SandboxToolCallDisposition, SandboxToolCallIntent,
};

#[derive(Default)]
struct RecordingProvider {
    requests: Mutex<Vec<ChatRequest>>,
}

#[async_trait]
impl ModelProvider for RecordingProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("recording")
    }

    async fn stream(&self, request: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        self.requests.lock().unwrap().push(request);
        Ok(stream::iter(vec![
            ProviderEvent::TextDelta {
                text: "done".into(),
            },
            ProviderEvent::Usage(Usage {
                input_tokens: 13,
                output_tokens: 5,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
            }),
            ProviderEvent::Stop {
                reason: StopReason::EndTurn,
            },
        ])
        .boxed())
    }
}

struct FixedResolver(Arc<dyn ModelProvider>);

#[async_trait]
impl ProviderResolver for FixedResolver {
    async fn resolve(&self) -> Arc<dyn ModelProvider> {
        self.0.clone()
    }
}

#[derive(Default)]
struct WebSearchThenFinalProvider {
    requests: Mutex<Vec<ChatRequest>>,
}

#[async_trait]
impl ModelProvider for WebSearchThenFinalProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("sandbox-web-search")
    }

    async fn stream(&self, request: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        let call_number = {
            let mut requests = self.requests.lock().unwrap();
            requests.push(request);
            requests.len()
        };
        let events = if call_number == 1 {
            vec![
                ProviderEvent::TextDelta {
                    text: "I’ll research that now.".into(),
                },
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
                    text: "search-informed answer".into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ]
        };
        Ok(stream::iter(events).boxed())
    }
}

/// A model that reaches for a tool the run was never offered, then answers
/// once it has been told what it may use.
#[derive(Default)]
struct UnavailableToolThenFinalProvider {
    requests: Mutex<Vec<ChatRequest>>,
}

#[async_trait]
impl ModelProvider for UnavailableToolThenFinalProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("sandbox-unavailable-tool")
    }

    async fn stream(&self, request: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        let call_number = {
            let mut requests = self.requests.lock().unwrap();
            requests.push(request);
            requests.len()
        };
        let events = if call_number == 1 {
            vec![
                ProviderEvent::ToolCallStarted {
                    index: 0,
                    id: "read_1".into(),
                    name: "read_file".into(),
                },
                ProviderEvent::ToolCallArgsDelta {
                    index: 0,
                    fragment: r#"{"path":"notes.md"}"#.into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::ToolUse,
                },
            ]
        } else {
            vec![
                ProviderEvent::TextDelta {
                    text: "answer without that file".into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ]
        };
        Ok(stream::iter(events).boxed())
    }
}

/// Two searches then a final answer, for the multi-checkpoint chain.
#[derive(Default)]
struct SearchTwiceThenFinalProvider {
    requests: Mutex<Vec<ChatRequest>>,
}

#[async_trait]
impl ModelProvider for SearchTwiceThenFinalProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("sandbox-web-search")
    }

    async fn stream(&self, request: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        let call_number = {
            let mut requests = self.requests.lock().unwrap();
            requests.push(request);
            requests.len()
        };
        let events = if call_number <= 2 {
            vec![
                ProviderEvent::ToolCallStarted {
                    index: 0,
                    id: format!("search_{call_number}"),
                    name: tidebreak_core::SANDBOX_WEB_SEARCH_TOOL.into(),
                },
                ProviderEvent::ToolCallArgsDelta {
                    index: 0,
                    fragment: format!(r#"{{"query":"Tidebreak {call_number}"}}"#),
                },
                ProviderEvent::Stop {
                    reason: StopReason::ToolUse,
                },
            ]
        } else {
            vec![
                ProviderEvent::TextDelta {
                    text: "answer from two searches".into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ]
        };
        Ok(stream::iter(events).boxed())
    }
}

/// One step that emits three calls at once: a command, a search, and a tool
/// the run was never offered. The step after it finishes the run.
#[derive(Default)]
struct ThreeCallStepThenFinalProvider {
    requests: Mutex<Vec<ChatRequest>>,
}

#[async_trait]
impl ModelProvider for ThreeCallStepThenFinalProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("sandbox-batch")
    }

    async fn stream(&self, request: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        let call_number = {
            let mut requests = self.requests.lock().unwrap();
            requests.push(request);
            requests.len()
        };
        let events = if call_number == 1 {
            vec![
                ProviderEvent::ToolCallStarted {
                    index: 0,
                    id: "exec_1".into(),
                    name: SANDBOX_EXEC_TOOL.into(),
                },
                ProviderEvent::ToolCallArgsDelta {
                    index: 0,
                    fragment: r#"{"command":"ls"}"#.into(),
                },
                ProviderEvent::ToolCallStarted {
                    index: 1,
                    id: "search_1".into(),
                    name: tidebreak_core::SANDBOX_WEB_SEARCH_TOOL.into(),
                },
                ProviderEvent::ToolCallArgsDelta {
                    index: 1,
                    fragment: r#"{"query":"Tidebreak"}"#.into(),
                },
                ProviderEvent::ToolCallStarted {
                    index: 2,
                    id: "read_1".into(),
                    name: "read_file".into(),
                },
                ProviderEvent::ToolCallArgsDelta {
                    index: 2,
                    fragment: r#"{"path":"notes.md"}"#.into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::ToolUse,
                },
            ]
        } else {
            vec![
                ProviderEvent::TextDelta {
                    text: "answer from one parallel step".into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ]
        };
        Ok(stream::iter(events).boxed())
    }
}

struct DelayedResolver {
    entered: Arc<Notify>,
    release: Arc<Notify>,
    provider: Arc<dyn ModelProvider>,
}

#[async_trait]
impl ProviderResolver for DelayedResolver {
    async fn resolve(&self) -> Arc<dyn ModelProvider> {
        self.entered.notify_one();
        self.release.notified().await;
        self.provider.clone()
    }
}

struct BlockingProvider {
    started: Arc<Notify>,
}

struct CountingPendingProvider {
    entries: Arc<AtomicUsize>,
}

struct DropMarker(Arc<AtomicBool>);

impl Drop for DropMarker {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

struct DropAwareResolver {
    entered: Arc<Notify>,
    dropped: Arc<AtomicBool>,
}

#[async_trait]
impl ProviderResolver for DropAwareResolver {
    async fn resolve(&self) -> Arc<dyn ModelProvider> {
        let _drop = DropMarker(self.dropped.clone());
        self.entered.notify_one();
        futures::future::pending().await
    }
}

struct DropAwareProvider {
    started: Arc<Notify>,
    dropped: Arc<AtomicBool>,
    first_event: Option<ProviderEvent>,
}

struct DropAwareStream {
    started: Arc<Notify>,
    dropped: Arc<AtomicBool>,
    first_event: Option<ProviderEvent>,
    announced: bool,
}

impl futures::Stream for DropAwareStream {
    type Item = ProviderEvent;

    fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if let Some(event) = self.first_event.take() {
            return Poll::Ready(Some(event));
        }
        if !self.announced {
            self.announced = true;
            self.started.notify_one();
        }
        Poll::Pending
    }
}

impl Drop for DropAwareStream {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}

#[async_trait]
impl ModelProvider for DropAwareProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("drop-aware")
    }

    async fn stream(&self, _request: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        Ok(DropAwareStream {
            started: self.started.clone(),
            dropped: self.dropped.clone(),
            first_event: self.first_event.clone(),
            announced: false,
        }
        .boxed())
    }
}

struct FailingProvider;

struct EventProvider(Vec<ProviderEvent>);

#[async_trait]
impl ModelProvider for EventProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("events")
    }

    async fn stream(&self, _request: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        Ok(stream::iter(self.0.clone()).boxed())
    }
}

#[async_trait]
impl ModelProvider for FailingProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("failing")
    }
    async fn stream(&self, _request: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        Err(AgentError::msg("provider unavailable"))
    }
}

#[async_trait]
impl ModelProvider for BlockingProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("blocking")
    }

    async fn stream(&self, _request: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        self.started.notify_one();
        Ok(stream::pending().boxed())
    }
}

#[async_trait]
impl ModelProvider for CountingPendingProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("counting-pending")
    }

    async fn stream(&self, _request: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        self.entries.fetch_add(1, Ordering::SeqCst);
        Ok(stream::pending().boxed())
    }
}

/// The sandbox worker resolves web search per run, which reads host
/// configuration and, for the automatic mode, whether a host provider has a
/// credential. These tests hold none, so an unconfigured host resolves the
/// host tool exactly as it did before this was resolved at all.
#[derive(Default)]
struct NoSecrets;

#[async_trait]
impl SecretProvider for NoSecrets {
    async fn get_secret(&self, _key: &str) -> Result<Option<String>> {
        Ok(None)
    }
    async fn set_secret(&self, _key: &str, _value: &str) -> Result<()> {
        Ok(())
    }
    async fn delete_secret(&self, _key: &str) -> Result<()> {
        Ok(())
    }
}

fn test_secrets() -> Arc<dyn SecretProvider> {
    Arc::new(NoSecrets)
}

async fn fixture() -> (
    SandboxAgentRunWorker,
    Arc<dyn Store>,
    Arc<RecordingProvider>,
    Chat,
    tempfile::TempDir,
) {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let chat = sandbox_chat();
    store.create_chat(&chat).await.unwrap();
    let provider = Arc::new(RecordingProvider::default());
    let worker = SandboxAgentRunWorker::new(
        store.clone(),
        test_secrets(),
        Arc::new(FixedResolver(provider.clone())),
        Arc::new(Notify::new()),
        Arc::new(Notify::new()),
        Arc::new(EventBus::default()),
        AgentConfig {
            model: "sandbox-model".into(),
            ..AgentConfig::default()
        },
        Some(dir.path().join("scratch")),
        SandboxAgentRunWorkerConfig::default(),
    );
    (worker, store, provider, chat, dir)
}

async fn admit_sandbox(
    store: &Arc<dyn Store>,
    chat_id: tidebreak_core::ChatId,
    call: CallId,
    input: &str,
) -> tidebreak_core::AgentRun {
    let running = store
        .list_turn_runs(chat_id)
        .await
        .unwrap()
        .into_iter()
        .find(|turn| turn.status == tidebreak_core::TurnRunStatus::Running);
    let (turn, lease) = if let Some(turn) = running {
        let lease = turn.lease_token.expect("running test turn has lease");
        (turn, lease)
    } else {
        let turn_id = tidebreak_core::TurnId::new();
        store
            .accept_turn(
                turn_id,
                chat_id,
                "sandbox-test-model",
                "sandbox test admission",
            )
            .await
            .unwrap();
        let lease = uuid::Uuid::new_v4();
        let now = Utc::now();
        let turn = store
            .claim_turn_run(lease, now, now + chrono::Duration::hours(1))
            .await
            .unwrap()
            .turn
            .expect("sandbox test turn should claim");
        (turn, lease)
    };
    match store
        .admit_sandbox_agent_run(
            turn.id,
            call,
            input,
            lease,
            turn.steer_revision,
            tidebreak_core::AgentRun::MAX_CONCURRENCY_LIMIT,
            Utc::now(),
        )
        .await
        .unwrap()
        .expect("sandbox test admission should resolve")
    {
        tidebreak_core::AdmitSandboxAgentRunOutcome::Accepted { child, .. }
        | tidebreak_core::AdmitSandboxAgentRunOutcome::Existing { child, .. } => child,
        outcome => panic!("unexpected sandbox admission: {outcome:?}"),
    }
}

/// Park one claimed foreground turn on an ordered child set, taking the
/// `TurnStarted` event the checkpoint's ordinal precondition requires.
async fn park_wait_set(
    store: &Arc<dyn Store>,
    wait_id: CallId,
    turn: &tidebreak_core::TurnRun,
    lease_token: uuid::Uuid,
    child_run_ids: &[tidebreak_core::AgentRunId],
) {
    store
        .append_turn_event(
            turn.chat_id,
            turn.id,
            lease_token,
            1,
            Utc::now(),
            &tidebreak_core::AgentEvent::TurnStarted { turn_id: turn.id },
        )
        .await
        .unwrap();
    store
        .park_turn_for_agent_run_wait_set(
            &tidebreak_core::AgentRunWaitSetCheckpointRequest {
                call_id: wait_id,
                origin_turn_id: turn.id,
                child_run_ids: child_run_ids.to_vec(),
                condition: tidebreak_core::AgentRunWaitCondition::All,
                lease_token,
                expected_steer_revision: turn.steer_revision,
                provider_id: format!("provider-{wait_id}"),
                arguments: serde_json::json!({"agent_ids": child_run_ids}),
                event_ordinal: 2,
                progress: TurnCheckpointProgress {
                    model_steps: 1,
                    usage: Usage::default(),
                },
            },
            Utc::now(),
        )
        .await
        .unwrap()
        .expect("foreground turn should park on its child set");
}

async fn ready_wait_set_for_test(
    store: &Arc<dyn Store>,
    chat_id: tidebreak_core::ChatId,
) -> (TurnId, CallId) {
    let turn_id = TurnId::new();
    store
        .accept_turn(turn_id, chat_id, "sandbox-model", "wait for one child")
        .await
        .unwrap();
    let foreground_lease = uuid::Uuid::new_v4();
    let now = Utc::now();
    let foreground = store
        .claim_turn_run(foreground_lease, now, now + chrono::Duration::minutes(1))
        .await
        .unwrap()
        .turn
        .unwrap();
    let child = admit_sandbox(store, chat_id, CallId::new(), "child").await;
    let wait_id = CallId::new();
    park_wait_set(store, wait_id, &foreground, foreground_lease, &[child.id]).await;
    let child_lease = uuid::Uuid::new_v4();
    let claimed = store
        .claim_agent_run(child_lease, chrono::Duration::minutes(1), 4, 4)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed.id, child.id);
    store
        .submit_agent_run_result(child.id, child_lease, "child result")
        .await
        .unwrap();
    (turn_id, wait_id)
}

#[tokio::test]
async fn completes_a_claimed_run_with_a_no_tools_private_request() {
    let (worker, store, provider, chat, dir) = fixture().await;
    let call = CallId::new();
    let id = tidebreak_core::AgentRunId::sandbox_for_spawn_call(call);
    let parent = tidebreak_core::AgentRunId::foreground_for_chat(chat.id);
    assert_eq!(
        admit_sandbox(&store, chat.id, call, "Investigate this in isolation.")
            .await
            .id,
        id
    );

    assert_eq!(
        worker.run_once().await.unwrap(),
        SandboxAgentRunWorkerOutcome::Completed(id)
    );
    let completed = store.get_agent_run(id).await.unwrap().unwrap();
    assert_eq!(completed.status, AgentRunStatus::Completed);
    assert_eq!(
        store.list_agent_run_inbox(parent).await.unwrap()[0]
            .result
            .text,
        "done"
    );
    let scratch = dir
        .path()
        .join("scratch")
        .join("sandbox-runs")
        .join(id.to_string());
    assert!(scratch.is_dir());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            scratch.metadata().unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    let requests = provider.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0]
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        vec![
            SANDBOX_EXEC_TOOL,
            tidebreak_core::SANDBOX_WEB_SEARCH_TOOL,
            tidebreak_core::UPDATE_TASK_PLAN_TOOL,
            tidebreak_core::SANDBOX_DONE_TOOL,
            tidebreak_core::REQUEST_FOLDER_ACCESS_TOOL,
        ]
    );
    assert_eq!(
        requests[0].messages,
        vec![ChatMessage::text(
            Role::User,
            "Investigate this in isolation."
        )]
    );
    assert_eq!(
        requests[0].system.as_deref(),
        Some(sandbox_system_prompt(false, TurnWebSearch::Host, true, &[], &[]).as_str())
    );
}

/// Regression: a sandbox child used to run the boot default model and never
/// carried the chat's reasoning effort, so a conversation on a cheaper model
/// was silently billed for the default one.
#[tokio::test]
async fn sandbox_run_inherits_the_chat_model_and_reasoning_effort() {
    let (worker, store, provider, _fixture_chat, _dir) = fixture().await;
    let chat = Chat {
        model: Some("chat-cheap-model".into()),
        reasoning_effort: Some(ReasoningEffort::Low),
        ..sandbox_chat()
    };
    store.create_chat(&chat).await.unwrap();

    // Mirror message acceptance: the turn freezes the chat's resolved model.
    let selected = crate::routes::resolve_chat_model(&*store, &chat, "boot-default-model")
        .await
        .unwrap();
    assert_eq!(selected, "chat-cheap-model");
    let turn_id = TurnId::new();
    store
        .accept_turn(turn_id, chat.id, &selected, "delegate")
        .await
        .unwrap();
    let lease = uuid::Uuid::new_v4();
    let now = Utc::now();
    store
        .claim_turn_run(lease, now, now + chrono::Duration::hours(1))
        .await
        .unwrap()
        .turn
        .expect("the delegating turn should claim");

    let run = admit_sandbox(
        &store,
        chat.id,
        CallId::new(),
        "Investigate this in isolation.",
    )
    .await;
    assert_eq!(run.model.as_deref(), Some("chat-cheap-model"));

    assert_eq!(
        worker.run_once().await.unwrap(),
        SandboxAgentRunWorkerOutcome::Completed(run.id)
    );
    assert_eq!(
        store
            .get_agent_run(run.id)
            .await
            .unwrap()
            .unwrap()
            .model
            .as_deref(),
        Some("chat-cheap-model")
    );
    let accounted = store.get_agent_run(run.id).await.unwrap().unwrap();
    assert_eq!(accounted.model_steps, 1);
    assert_eq!(
        accounted.usage,
        Usage {
            input_tokens: 13,
            output_tokens: 5,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
        }
    );
    let requests = provider.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].model, "chat-cheap-model");
    assert_eq!(requests[0].reasoning_effort, Some(ReasoningEffort::Low));
}

#[tokio::test]
async fn checkpoints_web_search_after_a_text_preamble_and_rebuilds_its_receipt() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let chat = sandbox_chat();
    store.create_chat(&chat).await.unwrap();
    let provider = Arc::new(WebSearchThenFinalProvider::default());
    let worker = SandboxAgentRunWorker::new(
        store.clone(),
        test_secrets(),
        Arc::new(FixedResolver(provider.clone())),
        Arc::new(Notify::new()),
        Arc::new(Notify::new()),
        Arc::new(EventBus::default()),
        AgentConfig {
            model: "sandbox-model".into(),
            max_steps: 2,
            ..AgentConfig::default()
        },
        None,
        SandboxAgentRunWorkerConfig::default(),
    );
    let spawn = CallId::new();
    let id = tidebreak_core::AgentRunId::sandbox_for_spawn_call(spawn);
    admit_sandbox(&store, chat.id, spawn, "Research this.").await;

    let call_id = match worker.run_once().await.unwrap() {
        SandboxAgentRunWorkerOutcome::ToolCheckpointed(call_id) => call_id,
        outcome => panic!("unexpected outcome: {outcome:?}"),
    };
    assert_eq!(
        store.get_agent_run(id).await.unwrap().unwrap().status,
        AgentRunStatus::Waiting
    );
    // The narration the model produced before checkpointing is published as
    // live progress, so the run is observable while it is still parked
    // rather than only once it submits a result.
    let progress = store.list_agent_run_progress(id, 0, 50).await.unwrap();
    assert_eq!(progress.len(), 1);
    assert_eq!(progress[0].sequence, 1);
    assert_eq!(progress[0].text, "I’ll research that now.");
    // Reading past the cursor the observer already holds returns nothing,
    // which is what makes polling cheap.
    assert!(store
        .list_agent_run_progress(id, progress[0].sequence, 50)
        .await
        .unwrap()
        .is_empty());
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

    assert_eq!(
        worker.run_once().await.unwrap(),
        SandboxAgentRunWorkerOutcome::Completed(id)
    );
    let requests = provider.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    // The last step cannot pay for another receipt, so the checkpointing
    // tools are withdrawn; submission and the folder-access proposal
    // terminate the run in place of a final answer and stay available.
    assert_eq!(
        requests[1]
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        vec![
            tidebreak_core::SANDBOX_DONE_TOOL,
            tidebreak_core::REQUEST_FOLDER_ACCESS_TOOL,
        ]
    );
    assert!(
        matches!(&requests[1].messages[1].content[0], ContentBlock::ToolUse { id, name, input } if id == "search_1" && name == tidebreak_core::SANDBOX_WEB_SEARCH_TOOL && input == &serde_json::json!({"query":"Tidebreak"}))
    );
    assert!(
        matches!(&requests[1].messages[2].content[0], ContentBlock::ToolResult { tool_use_id, content, is_error } if tool_use_id == "search_1" && content == "{\"results\":[]}" && !is_error)
    );
}

/// The failure this whole path exists to prevent: a background run that
/// reaches for a tool it was never offered used to fail its attempt, and
/// because every retry replays the same context it failed the same way
/// until the budget ran out. The call is answered instead, and the run
/// carries on from the same attempt.
#[tokio::test]
async fn an_unavailable_tool_call_is_answered_and_the_run_keeps_its_attempt() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let chat = sandbox_chat();
    store.create_chat(&chat).await.unwrap();
    let provider = Arc::new(UnavailableToolThenFinalProvider::default());
    let worker = SandboxAgentRunWorker::new(
        store.clone(),
        test_secrets(),
        Arc::new(FixedResolver(provider.clone())),
        Arc::new(Notify::new()),
        Arc::new(Notify::new()),
        Arc::new(EventBus::default()),
        AgentConfig {
            model: "sandbox-model".into(),
            max_steps: 3,
            ..AgentConfig::default()
        },
        None,
        SandboxAgentRunWorkerConfig::default(),
    );
    let spawn = CallId::new();
    let id = tidebreak_core::AgentRunId::sandbox_for_spawn_call(spawn);
    admit_sandbox(&store, chat.id, spawn, "Summarize the notes.").await;

    let call_id = match worker.run_once().await.unwrap() {
        SandboxAgentRunWorkerOutcome::ToolCheckpointed(call_id) => call_id,
        outcome => panic!("unexpected outcome: {outcome:?}"),
    };
    let parked = store.get_agent_run(id).await.unwrap().unwrap();
    // Not `waiting`: no executor lane will ever resolve this call, so the
    // run has to be immediately claimable again or it would hang forever.
    assert_eq!(parked.status, AgentRunStatus::RetryWait);
    assert_eq!(
        parked.last_error_code.as_deref(),
        Some("tool_checkpoint_resolved")
    );
    assert_eq!(parked.attempt_count, 1);
    let receipt = store
        .get_sandbox_tool_call_receipt(call_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(receipt.status, SandboxToolCallStatus::Failed);
    assert!(receipt.result.contains("Available tools:"), "{receipt:?}");
    // The call is answered, so no executor lane may pick it up.
    assert!(store
        .list_sandbox_tool_call_candidates(10)
        .await
        .unwrap()
        .is_empty());

    assert_eq!(
        worker.run_once().await.unwrap(),
        SandboxAgentRunWorkerOutcome::Completed(id)
    );
    let after = store.get_agent_run(id).await.unwrap().unwrap();
    assert_eq!(after.attempt_count, 1);
    let requests = provider.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(
        matches!(&requests[1].messages[1].content[0], ContentBlock::ToolUse { id, name, input } if id == "read_1" && name == "read_file" && input == &serde_json::json!({"path":"notes.md"}))
    );
    assert!(
        matches!(&requests[1].messages[2].content[0], ContentBlock::ToolResult { tool_use_id, content, is_error } if tool_use_id == "read_1" && content.contains("Available tools:") && *is_error)
    );
}

/// A run whose task needs more than one lookup: the checkpoint chain has to
/// survive a second park and replay both receipts in order, because the
/// worker rebuilds the whole transcript from the database on every claim.
#[tokio::test]
async fn a_run_checkpoints_more_than_once_and_replays_every_receipt_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let chat = sandbox_chat();
    store.create_chat(&chat).await.unwrap();
    let provider = Arc::new(SearchTwiceThenFinalProvider::default());
    let worker = SandboxAgentRunWorker::new(
        store.clone(),
        test_secrets(),
        Arc::new(FixedResolver(provider.clone())),
        Arc::new(Notify::new()),
        Arc::new(Notify::new()),
        Arc::new(EventBus::default()),
        AgentConfig {
            model: "sandbox-model".into(),
            max_steps: 4,
            ..AgentConfig::default()
        },
        None,
        SandboxAgentRunWorkerConfig::default(),
    );
    let spawn = CallId::new();
    let id = tidebreak_core::AgentRunId::sandbox_for_spawn_call(spawn);
    admit_sandbox(&store, chat.id, spawn, "Research this thoroughly.").await;

    for result in ["{\"results\":[\"first\"]}", "{\"results\":[\"second\"]}"] {
        let call_id = match worker.run_once().await.unwrap() {
            SandboxAgentRunWorkerOutcome::ToolCheckpointed(call_id) => call_id,
            outcome => panic!("unexpected outcome: {outcome:?}"),
        };
        assert_eq!(
            store.get_agent_run(id).await.unwrap().unwrap().status,
            AgentRunStatus::Waiting
        );
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
                    result: result.into(),
                },
            )
            .await
            .unwrap();
    }

    assert_eq!(
        worker.run_once().await.unwrap(),
        SandboxAgentRunWorkerOutcome::Completed(id)
    );
    let requests = provider.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    let replayed: Vec<&str> = requests[2]
        .messages
        .iter()
        .filter_map(|message| match message.content.first() {
            Some(ContentBlock::ToolResult { content, .. }) => Some(content.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        replayed,
        vec!["{\"results\":[\"first\"]}", "{\"results\":[\"second\"]}"]
    );
}

/// A step that asks for several things at once. The whole point of the
/// batch: three calls park together, the run stays parked until the last
/// live one is answered, and the next request replays the step as the model
/// produced it — one assistant message holding all three tool uses, one
/// message holding all three results, in emission order.
#[tokio::test]
async fn one_step_parks_its_calls_as_a_batch_and_replays_them_together() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let chat = sandbox_chat();
    store.create_chat(&chat).await.unwrap();
    let provider = Arc::new(ThreeCallStepThenFinalProvider::default());
    let worker = SandboxAgentRunWorker::new(
        store.clone(),
        test_secrets(),
        Arc::new(FixedResolver(provider.clone())),
        Arc::new(Notify::new()),
        Arc::new(Notify::new()),
        Arc::new(EventBus::default()),
        AgentConfig {
            model: "sandbox-model".into(),
            max_steps: 4,
            ..AgentConfig::default()
        },
        None,
        SandboxAgentRunWorkerConfig::default(),
    );
    let spawn = CallId::new();
    let id = tidebreak_core::AgentRunId::sandbox_for_spawn_call(spawn);
    admit_sandbox(&store, chat.id, spawn, "Do three things.").await;

    assert!(matches!(
        worker.run_once().await.unwrap(),
        SandboxAgentRunWorkerOutcome::ToolCheckpointed(_)
    ));
    let parked = store
        .list_sandbox_tool_calls_for_agent_run(id)
        .await
        .unwrap();
    assert_eq!(
        parked
            .iter()
            .map(|call| (call.batch_ordinal, call.name.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (0, SANDBOX_EXEC_TOOL),
            (1, tidebreak_core::SANDBOX_WEB_SEARCH_TOOL),
            (2, "read_file"),
        ]
    );
    // The unadvertised call is answered inside the same park, so it lands
    // terminal with its receipt already attached and no lane ever sees it.
    assert!(parked[2].status.is_terminal());
    let refusal = store
        .get_sandbox_tool_call_receipt(parked[2].id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(refusal.status, SandboxToolCallStatus::Failed);
    assert_eq!(
        store.get_agent_run(id).await.unwrap().unwrap().status,
        AgentRunStatus::Waiting
    );

    for (index, result) in [(0, "exec output"), (1, "{\"results\":[]}")] {
        let executor_lease = uuid::Uuid::new_v4();
        store
            .claim_sandbox_tool_call(
                parked[index].id,
                executor_lease,
                chrono::Duration::minutes(1),
            )
            .await
            .unwrap();
        store
            .resolve_sandbox_tool_call(
                parked[index].id,
                executor_lease,
                &ToolCallResolution::Completed {
                    result: result.into(),
                },
            )
            .await
            .unwrap();
        // The run resumes on the last receipt of the step, not the first.
        let expected = if index == 0 {
            AgentRunStatus::Waiting
        } else {
            AgentRunStatus::RetryWait
        };
        assert_eq!(
            store.get_agent_run(id).await.unwrap().unwrap().status,
            expected
        );
    }

    assert_eq!(
        worker.run_once().await.unwrap(),
        SandboxAgentRunWorkerOutcome::Completed(id)
    );
    let after = store.get_agent_run(id).await.unwrap().unwrap();
    assert_eq!(after.attempt_count, 1);
    let requests = provider.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    let replay = &requests[1].messages;
    let uses: Vec<&str> = replay[1]
        .content
        .iter()
        .map(|block| match block {
            ContentBlock::ToolUse { id, .. } => id.as_str(),
            other => panic!("unexpected assistant block: {other:?}"),
        })
        .collect();
    assert_eq!(uses, vec!["exec_1", "search_1", "read_1"]);
    let results: Vec<(&str, bool)> = replay[2]
        .content
        .iter()
        .map(|block| match block {
            ContentBlock::ToolResult {
                tool_use_id,
                is_error,
                ..
            } => (tool_use_id.as_str(), *is_error),
            other => panic!("unexpected result block: {other:?}"),
        })
        .collect();
    assert_eq!(
        results,
        vec![("exec_1", false), ("search_1", false), ("read_1", true)]
    );
    assert_eq!(replay.len(), 3);
}

#[tokio::test]
async fn refuses_malformed_sandbox_tool_events_without_checkpointing() {
    let request = ChatRequest {
        provider: None,
        model: "m".into(),
        reasoning_model: false,
        system: None,
        messages: vec![],
        tools: vec![],
        max_tokens: None,
        temperature: None,
        reasoning_effort: None,
        images: tidebreak_core::ImageAttachments::new(),
        ..Default::default()
    };
    for events in [
        vec![
            ProviderEvent::ToolCallStarted {
                index: 0,
                id: "one".into(),
                name: "web_search".into(),
            },
            ProviderEvent::Stop {
                reason: StopReason::EndTurn,
            },
        ],
        vec![
            ProviderEvent::ToolCallStarted {
                index: 0,
                id: "one".into(),
                name: "web_search".into(),
            },
            ProviderEvent::ToolCallArgsDelta {
                index: 0,
                fragment: "x".repeat(ToolCallRecord::MAX_ARGUMENT_BYTES + 1),
            },
            ProviderEvent::Stop {
                reason: StopReason::ToolUse,
            },
        ],
        vec![
            ProviderEvent::ToolCallStarted {
                index: 0,
                id: String::new(),
                name: "web_search".into(),
            },
            ProviderEvent::Stop {
                reason: StopReason::ToolUse,
            },
        ],
    ] {
        assert!(
            complete_sandbox_task(Arc::new(EventProvider(events)), request.clone())
                .await
                .outcome
                .is_err()
        );
    }
}

#[tokio::test]
async fn sandbox_provider_receipts_require_vendor_entitlement_canonical_shape_and_route() {
    let dir = tempfile::tempdir().unwrap();
    let store = DbStore::connect(&format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("t.db").display()
    ))
    .await
    .unwrap();
    let vendor = tidebreak_core::VendorWebSearch { max_uses: 3 };
    let canonical_input = serde_json::json!({"query": "Tidebreak release notes"});
    let canonical_output = serde_json::json!({
        "provider": "anthropic",
        "results": [{
            "url": "https://example.com/notes",
            "title": "Release notes",
            "snippet": "What shipped",
        }],
    });
    let usage = Usage {
        input_tokens: 17,
        output_tokens: 4,
        cache_read_input_tokens: 2,
        cache_creation_input_tokens: 1,
    };
    let request_for = |web_search| AgentConfig {
        provider: Some(ProviderId::new("anthropic")),
        model: "claude-opus-5".into(),
        web_search,
        ..AgentConfig::default()
    };
    let vendor_request = sandbox_request(
        &request_for(TurnWebSearch::Vendor(vendor)),
        "task".into(),
        &[],
        &store,
        false,
        &[],
        &[],
    )
    .await
    .unwrap();
    let off_request = sandbox_request(
        &request_for(TurnWebSearch::Off),
        "task".into(),
        &[],
        &store,
        false,
        &[],
        &[],
    )
    .await
    .unwrap();
    let host_request = sandbox_request(
        &request_for(TurnWebSearch::Host),
        "task".into(),
        &[],
        &store,
        false,
        &[],
        &[],
    )
    .await
    .unwrap();
    let receipt = |name: &str,
                   input: serde_json::Value,
                   output: serde_json::Value,
                   replay: Option<tidebreak_core::ProviderToolReplay>| {
        ProviderEvent::ProviderExecutedToolCall {
            name: name.into(),
            input,
            output,
            is_error: false,
            replay,
        }
    };
    let mismatched_replay = tidebreak_core::ProviderToolReplay::captured(
        tidebreak_core::ReasoningOrigin {
            provider: Some(ProviderId::new("anthropic")),
            model: "different-model".into(),
        },
        vec![
            serde_json::json!({
                "type": "server_tool_use",
                "id": "srvtoolu_search",
                "name": tidebreak_core::WEB_SEARCH_TOOL,
                "input": canonical_input.clone(),
            }),
            serde_json::json!({
                "type": "web_search_tool_result",
                "tool_use_id": "srvtoolu_search",
                "content": [{"encrypted_content": "opaque"}],
            }),
        ],
    );
    let rejected = vec![
        (
            "fake provider exec",
            vendor_request.clone(),
            receipt(
                SANDBOX_EXEC_TOOL,
                serde_json::json!({"command": "echo"}),
                serde_json::json!({"stdout": "forged"}),
                None,
            ),
        ),
        (
            "web search while off",
            off_request,
            receipt(
                tidebreak_core::WEB_SEARCH_TOOL,
                canonical_input.clone(),
                canonical_output.clone(),
                None,
            ),
        ),
        (
            "web search while host-routed",
            host_request,
            receipt(
                tidebreak_core::WEB_SEARCH_TOOL,
                canonical_input.clone(),
                canonical_output.clone(),
                None,
            ),
        ),
        (
            "malformed normalized output",
            vendor_request.clone(),
            receipt(
                tidebreak_core::WEB_SEARCH_TOOL,
                canonical_input.clone(),
                serde_json::json!({"results": []}),
                None,
            ),
        ),
        (
            "oversized normalized input",
            vendor_request.clone(),
            receipt(
                tidebreak_core::WEB_SEARCH_TOOL,
                serde_json::json!({"query": "x".repeat(tidebreak_core::MAX_WEB_SEARCH_QUERY_CHARS + 1)}),
                canonical_output.clone(),
                None,
            ),
        ),
        (
            "oversized normalized output",
            vendor_request.clone(),
            receipt(
                tidebreak_core::WEB_SEARCH_TOOL,
                canonical_input.clone(),
                serde_json::json!({
                    "provider": "anthropic",
                    "results": [{
                        "url": "https://example.com/notes",
                        "title": "Release notes",
                        "snippet": "What shipped",
                        "content": "x".repeat(16_000),
                    }],
                }),
                None,
            ),
        ),
        (
            "mismatched native replay origin",
            vendor_request.clone(),
            receipt(
                tidebreak_core::WEB_SEARCH_TOOL,
                canonical_input.clone(),
                canonical_output.clone(),
                Some(mismatched_replay),
            ),
        ),
    ];

    for (label, request, event) in rejected {
        let attempt = complete_sandbox_task(
            Arc::new(EventProvider(vec![
                event,
                ProviderEvent::Usage(usage),
                ProviderEvent::TextDelta {
                    text: "answer".into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ])),
            request,
        )
        .await;
        assert_eq!(attempt.usage, usage, "{label} must not erase usage");
        assert!(attempt.account, "{label} must remain accounted");
        let step = attempt.outcome.unwrap();
        assert!(
            step.provider_executed.is_empty(),
            "{label} must not publish provider progress: {:?}",
            step.provider_executed
        );
    }

    let matching_replay = tidebreak_core::ProviderToolReplay::captured(
        tidebreak_core::ReasoningOrigin {
            provider: Some(ProviderId::new("anthropic")),
            model: "claude-opus-5".into(),
        },
        vec![
            serde_json::json!({
                "type": "server_tool_use",
                "id": "srvtoolu_search",
                "name": tidebreak_core::WEB_SEARCH_TOOL,
                "input": canonical_input.clone(),
            }),
            serde_json::json!({
                "type": "web_search_tool_result",
                "tool_use_id": "srvtoolu_search",
                "content": [{"encrypted_content": "opaque"}],
            }),
        ],
    );
    let accepted = complete_sandbox_task(
        Arc::new(EventProvider(vec![
            receipt(
                tidebreak_core::WEB_SEARCH_TOOL,
                canonical_input,
                canonical_output,
                Some(matching_replay),
            ),
            ProviderEvent::TextDelta {
                text: "answer".into(),
            },
            ProviderEvent::Stop {
                reason: StopReason::EndTurn,
            },
        ])),
        vendor_request,
    )
    .await
    .outcome
    .unwrap();
    assert_eq!(accepted.provider_executed.len(), 1);
    assert_eq!(
        accepted.provider_executed[0].progress_line(),
        "Ran web_search: Tidebreak release notes"
    );
}

/// A tool the run was never offered is the model's mistake, not the
/// host's: it is answered in the transcript so the next step can correct
/// itself, rather than spending the run's attempt budget.
#[tokio::test]
async fn unadvertised_sandbox_tool_call_is_answered_rather_than_refused() {
    let request = ChatRequest {
        provider: None,
        model: "m".into(),
        reasoning_model: false,
        system: None,
        messages: vec![],
        tools: vec![sandbox_exec_tool_spec(), sandbox_done_tool_spec()],
        max_tokens: None,
        temperature: None,
        reasoning_effort: None,
        images: tidebreak_core::ImageAttachments::new(),
        ..Default::default()
    };
    let completion = complete_sandbox_task(
        Arc::new(EventProvider(vec![
            ProviderEvent::ToolCallStarted {
                index: 0,
                id: "one".into(),
                name: "read_file".into(),
            },
            ProviderEvent::ToolCallArgsDelta {
                index: 0,
                fragment: "{}".into(),
            },
            ProviderEvent::Stop {
                reason: StopReason::ToolUse,
            },
        ])),
        request,
    )
    .await
    .outcome
    .unwrap()
    .completion;
    let SandboxCompletion::ToolCalls(intents) = completion else {
        panic!("an unadvertised tool must still produce a tool call");
    };
    let [intent] = <[_; 1]>::try_from(intents).expect("one call");
    assert_eq!(intent.name, "read_file");
    let SandboxToolCallDisposition::Rejected {
        error_code,
        message,
    } = intent.disposition
    else {
        panic!("an unadvertised tool must not be dispatched");
    };
    assert_eq!(error_code, "unavailable_tool");
    assert!(message.contains("Available tools:"), "{message}");
    assert!(message.contains(SANDBOX_EXEC_TOOL), "{message}");
}

#[tokio::test]
async fn sandbox_completion_treats_bare_and_structured_refusals_as_failures() {
    let request = ChatRequest {
        provider: None,
        model: "m".into(),
        reasoning_model: false,
        system: None,
        messages: vec![],
        tools: vec![],
        max_tokens: None,
        temperature: None,
        reasoning_effort: None,
        images: tidebreak_core::ImageAttachments::new(),
        ..Default::default()
    };

    let bare = match complete_sandbox_task(
        Arc::new(EventProvider(vec![ProviderEvent::Stop {
            reason: StopReason::Refusal,
        }])),
        request.clone(),
    )
    .await
    .outcome
    {
        Err(error) => error,
        Ok(_) => panic!("bare refusal must not complete a sandbox run"),
    };
    assert!(
        matches!(bare, AgentError::Refusal(ref detail) if detail.contains("unspecified")),
        "{bare}"
    );

    let structured = match complete_sandbox_task(
        Arc::new(EventProvider(vec![
            ProviderEvent::TextDelta {
                text: "unsafe partial".into(),
            },
            ProviderEvent::Refusal {
                details: tidebreak_core::RefusalDetails::from_category(Some("cyber")),
            },
        ])),
        request,
    )
    .await
    .outcome
    {
        Err(error) => error,
        Ok(_) => panic!("structured refusal must not complete a sandbox run"),
    };
    assert!(
        matches!(structured, AgentError::Refusal(ref detail) if detail.contains("cyber")),
        "{structured}"
    );
}

#[tokio::test]
async fn cancellation_fence_prevents_a_stale_worker_from_parking_web_search() {
    let (worker, store, _provider, chat, _dir) = fixture().await;
    let spawn = CallId::new();
    let id = tidebreak_core::AgentRunId::sandbox_for_spawn_call(spawn);
    admit_sandbox(&store, chat.id, spawn, "Research this.").await;
    let lease = uuid::Uuid::new_v4();
    let run = store
        .claim_agent_run(lease, chrono::Duration::minutes(1), 4, 2)
        .await
        .unwrap()
        .unwrap();
    store.request_agent_run_cancellation(id).await.unwrap();
    assert_eq!(
        worker
            .park_sandbox_tool_calls(
                run,
                lease,
                vec![SandboxToolCallIntent {
                    provider_id: "search_1".into(),
                    name: tidebreak_core::SANDBOX_WEB_SEARCH_TOOL.into(),
                    arguments: serde_json::json!({"query":"Tidebreak"}),
                    disposition: SandboxToolCallDisposition::Execute,
                }],
                0,
                100,
                "",
            )
            .await
            .unwrap(),
        SandboxAgentRunWorkerOutcome::Cancelled(id),
    );
    assert!(store
        .list_sandbox_tool_calls_for_agent_run(id)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn durable_wait_set_scan_resumes_and_publishes_without_a_wake() {
    let (worker, store, _provider, chat, _dir) = fixture().await;
    let mut live_events = worker.events.subscribe(chat.id);
    let (turn_id, wait_id) = ready_wait_set_for_test(&store, chat.id).await;

    assert_eq!(
        worker.run_once().await.unwrap(),
        SandboxAgentRunWorkerOutcome::ParentWaitSetResumed(wait_id)
    );
    let published = tokio::time::timeout(Duration::from_secs(1), live_events.recv())
        .await
        .expect("committed wait event should publish live")
        .expect("live wait event channel should remain open");
    let durable = store.list_events(chat.id, 1).await.unwrap();
    assert_eq!(durable, vec![published]);
    tokio::time::timeout(Duration::from_secs(1), worker.turn_wake.notified())
        .await
        .expect("resumed wait should wake the turn worker");
    assert_eq!(
        store.get_turn_run(turn_id).await.unwrap().unwrap().status,
        TurnRunStatus::Resuming
    );

    let resume_token = store
        .list_agent_run_inbox(tidebreak_core::AgentRunId::foreground_for_chat(chat.id))
        .await
        .unwrap()
        .into_iter()
        .find_map(|entry| entry.consumed_lease_token)
        .expect("resumed wait should preserve its exact resume token");
    assert_eq!(
        worker
            .resume_parent_wait_set_with_token(wait_id, resume_token)
            .await
            .unwrap(),
        SandboxAgentRunWorkerOutcome::Idle
    );
    assert!(matches!(
        live_events.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
}

#[tokio::test]
async fn ambiguous_wait_set_resume_retries_exactly_then_publishes_and_wakes() {
    let (worker, store, _provider, chat, _dir) = fixture().await;
    let mut live_events = worker.events.subscribe(chat.id);
    let (turn_id, wait_id) = ready_wait_set_for_test(&store, chat.id).await;
    worker
        .fail_wait_set_resume_responses
        .store(2, Ordering::SeqCst);
    // Let the first exact retry skip its backoff after the injected error.
    worker.wake.notify_one();

    assert_eq!(
        worker.run_once().await.unwrap(),
        SandboxAgentRunWorkerOutcome::ParentWaitSetResumed(wait_id)
    );
    let published = tokio::time::timeout(Duration::from_secs(1), live_events.recv())
        .await
        .expect("ambiguous wait recovery should publish live")
        .expect("live wait event channel should remain open");
    assert_eq!(
        store.list_events(chat.id, 1).await.unwrap(),
        vec![published]
    );
    tokio::time::timeout(Duration::from_secs(1), worker.turn_wake.notified())
        .await
        .expect("ambiguous wait recovery should wake the turn worker");
    assert_eq!(
        store.get_turn_run(turn_id).await.unwrap().unwrap().status,
        TurnRunStatus::Resuming
    );
    assert_eq!(
        worker.fail_wait_set_resume_responses.load(Ordering::SeqCst),
        0
    );
}

/// A run's deliverables are the files it wrote, so submission has exactly two
/// jobs: carry the names the model chose through to the parent, and never
/// let a name the run did not produce — whether nothing in the conversation
/// carries it, or something does but another writer put it there — pass as
/// a deliverable. `done` is a terminal tool, not a checkpoint, so failing
/// the whole submission over one bad name would schedule a retry that
/// replays byte-identical context: the model would repeat the same wrong
/// name until its attempts ran out, discarding files it did produce along
/// the way even though they are already sitting in the outputs catalog.
/// Submission instead carries every name that resolved and reports the
/// rest in the receipt, so a bad name costs nothing but itself.
#[tokio::test]
async fn submitting_files_carries_resolved_names_and_reports_the_rest_without_failing_the_run() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let chat = sandbox_chat();
    store.create_chat(&chat).await.unwrap();
    let call = CallId::new();
    let id = tidebreak_core::AgentRunId::sandbox_for_spawn_call(call);
    let published = tidebreak_core::OutputId::new();
    let seed = |output_id, filename: &str, run: Option<tidebreak_core::AgentRunId>| {
        let request = tidebreak_core::CreateOutput {
            id: output_id,
            chat_id: chat.id,
            filename: filename.to_owned(),
            kind: tidebreak_core::DeliverableKind::Text,
            revision: tidebreak_core::NewOutputRevision {
                id: tidebreak_core::OutputRevisionId::new(),
                byte_len: 4,
                sha256: [0; 32],
                turn_id: None,
                producing_run_id: run,
                created_at: chrono::Utc::now(),
            },
        };
        let store = store.clone();
        async move { store.create_output(&request).await.unwrap() }
    };
    // What this run wrote, and what somebody else's writer left in the same
    // conversation under a name the run might reach for.
    seed(published, "Q3 revenue.md", Some(id)).await;
    seed(tidebreak_core::OutputId::new(), "Q4 revenue.md", None).await;

    let done = |filenames: &[&str]| {
        Arc::new(EventProvider(vec![
            ProviderEvent::ToolCallStarted {
                index: 0,
                id: "done_1".into(),
                name: tidebreak_core::SANDBOX_DONE_TOOL.into(),
            },
            ProviderEvent::ToolCallArgsDelta {
                index: 0,
                fragment: serde_json::json!({
                    "outputs": filenames,
                    "summary": "Wrote the revenue summary.",
                })
                .to_string(),
            },
            ProviderEvent::Stop {
                reason: StopReason::ToolUse,
            },
        ]))
    };
    let worker = |provider: Arc<EventProvider>| {
        SandboxAgentRunWorker::new(
            store.clone(),
            test_secrets(),
            Arc::new(FixedResolver(provider)),
            Arc::new(Notify::new()),
            Arc::new(Notify::new()),
            Arc::new(EventBus::default()),
            AgentConfig {
                model: "sandbox-model".into(),
                ..AgentConfig::default()
            },
            None,
            SandboxAgentRunWorkerConfig::default(),
        )
    };

    admit_sandbox(&store, chat.id, call, "Summarize Q3 revenue.").await;
    assert_eq!(
        worker(done(&["Q3 revenue.md"])).run_once().await.unwrap(),
        SandboxAgentRunWorkerOutcome::Completed(id)
    );
    let result = store.get_agent_run_result(id).await.unwrap().unwrap();
    assert!(matches!(
        &result.payload,
        tidebreak_core::AgentRunResultPayload::Submission { outputs, summary }
            if outputs.len() == 1
                && outputs[0].output_id == published
                && outputs[0].filename == "Q3 revenue.md"
                && summary == "Wrote the revenue summary."
    ));
    // The parent reads the filenames, because the files are the result.
    assert!(result.text.contains("Q3 revenue.md"));

    // A second run names a file it wrote alongside one it never wrote —
    // "Q4 revenue.md" belongs to nobody's run above, and "Q3 revenue.md"
    // belongs to the first run, not this one.
    let mixed = CallId::new();
    let mixed_id = tidebreak_core::AgentRunId::sandbox_for_spawn_call(mixed);
    let own = tidebreak_core::OutputId::new();
    seed(own, "Q4 revenue (final).md", Some(mixed_id)).await;
    admit_sandbox(&store, chat.id, mixed, "Summarize Q4 revenue.").await;
    assert_eq!(
        worker(done(&[
            "Q4 revenue (final).md",
            "Q4 revenue.md",
            "Q3 revenue.md"
        ]))
        .run_once()
        .await
        .unwrap(),
        SandboxAgentRunWorkerOutcome::Completed(mixed_id)
    );
    let mixed_result = store.get_agent_run_result(mixed_id).await.unwrap().unwrap();
    assert!(matches!(
        &mixed_result.payload,
        tidebreak_core::AgentRunResultPayload::Submission { outputs, summary }
            if outputs.len() == 1
                && outputs[0].output_id == own
                && outputs[0].filename == "Q4 revenue (final).md"
                && summary.contains("Wrote the revenue summary.")
                // The bad names are reported, never silently dropped.
                && summary.contains("Q4 revenue.md")
                && summary.contains("Q3 revenue.md")
    ));
}

#[tokio::test]
async fn folder_proposal_completes_the_child_then_resumes_the_parent_without_a_tool_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let chat = sandbox_chat();
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    store
        .accept_turn(turn_id, chat.id, "sandbox-model", "delegate")
        .await
        .unwrap();
    let foreground_lease = uuid::Uuid::new_v4();
    let now = chrono::Utc::now();
    let foreground = store
        .claim_turn_run(foreground_lease, now, now + chrono::Duration::minutes(1))
        .await
        .unwrap()
        .turn
        .unwrap();
    let call = CallId::new();
    let child = admit_sandbox(&store, chat.id, call, "ask whether a folder is needed").await;
    let child_id = child.id;
    let wait_id = CallId::new();
    park_wait_set(&store, wait_id, &foreground, foreground_lease, &[child_id]).await;
    let provider = Arc::new(EventProvider(vec![
            ProviderEvent::ToolCallStarted {
                index: 0,
                id: "folder_1".into(),
                name: tidebreak_core::REQUEST_FOLDER_ACCESS_TOOL.into(),
            },
            ProviderEvent::ToolCallArgsDelta {
                index: 0,
                fragment: r#"{"reason":"Read documents needed for the task","requested_capabilities":["read_files"],"folder_hint":"documents"}"#.into(),
            },
            ProviderEvent::Stop { reason: StopReason::ToolUse },
        ]));
    let worker = SandboxAgentRunWorker::new(
        store.clone(),
        test_secrets(),
        Arc::new(FixedResolver(provider)),
        Arc::new(Notify::new()),
        Arc::new(Notify::new()),
        Arc::new(EventBus::default()),
        AgentConfig {
            model: "sandbox-model".into(),
            max_steps: 2,
            ..AgentConfig::default()
        },
        None,
        SandboxAgentRunWorkerConfig::default(),
    );
    assert_eq!(
        worker.run_once().await.unwrap(),
        SandboxAgentRunWorkerOutcome::Completed(child_id)
    );
    assert!(store
        .list_sandbox_tool_calls_for_agent_run(child_id)
        .await
        .unwrap()
        .is_empty());
    let inbox = store
        .list_agent_run_inbox(tidebreak_core::AgentRunId::foreground_for_chat(chat.id))
        .await
        .unwrap();
    assert!(matches!(
        &inbox[0].result.payload,
        tidebreak_core::AgentRunResultPayload::FolderAccessProposal { request }
            if request.reason == "Read documents needed for the task"
    ));
    assert_eq!(
        worker.run_once().await.unwrap(),
        SandboxAgentRunWorkerOutcome::ParentWaitSetResumed(wait_id)
    );
}

#[tokio::test]
async fn acknowledges_cancellation_under_its_exact_live_lease() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let chat = sandbox_chat();
    store.create_chat(&chat).await.unwrap();
    let call = CallId::new();
    let id = tidebreak_core::AgentRunId::sandbox_for_spawn_call(call);
    assert_eq!(
        admit_sandbox(&store, chat.id, call, "Wait until cancelled.")
            .await
            .id,
        id
    );
    let started = Arc::new(Notify::new());
    let worker = SandboxAgentRunWorker::new(
        store.clone(),
        test_secrets(),
        Arc::new(FixedResolver(Arc::new(BlockingProvider {
            started: started.clone(),
        }))),
        Arc::new(Notify::new()),
        Arc::new(Notify::new()),
        Arc::new(EventBus::default()),
        AgentConfig {
            model: "sandbox-model".into(),
            ..AgentConfig::default()
        },
        None,
        SandboxAgentRunWorkerConfig {
            heartbeat: Duration::from_millis(10),
            ..SandboxAgentRunWorkerConfig::default()
        },
    );
    let started_wait = started.notified();
    let execution = tokio::spawn(async move { worker.run_once().await });
    started_wait.await;
    assert!(matches!(
        store.request_agent_run_cancellation(id).await.unwrap(),
        Some(tidebreak_core::RequestAgentRunCancellationOutcome::Requested(_))
    ));
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), execution)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        SandboxAgentRunWorkerOutcome::Cancelled(id)
    );
    assert_eq!(
        store.get_agent_run(id).await.unwrap().unwrap().status,
        AgentRunStatus::Cancelled
    );
}

#[tokio::test]
async fn immediate_failures_release_capacity_then_deliver_a_terminal_parent_receipt() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let chat = sandbox_chat();
    store.create_chat(&chat).await.unwrap();
    let store_for_child = store.clone();
    let child = move |task: String| {
        let store = store_for_child.clone();
        async move {
            let call = CallId::new();
            let id = tidebreak_core::AgentRunId::sandbox_for_spawn_call(call);
            admit_sandbox(&store, chat.id, call, &task).await;
            id
        }
    };
    let first = child("first".into()).await;
    let worker = |retry| {
        SandboxAgentRunWorker::new(
            store.clone(),
            test_secrets(),
            Arc::new(FixedResolver(Arc::new(FailingProvider))),
            Arc::new(Notify::new()),
            Arc::new(Notify::new()),
            Arc::new(EventBus::default()),
            AgentConfig {
                model: "m".into(),
                ..AgentConfig::default()
            },
            None,
            SandboxAgentRunWorkerConfig {
                failure_delay: Duration::from_secs(1),
                retry,
                max_concurrency: 1,
                max_running_global: 1,
                max_running_per_chat: 1,
                ..SandboxAgentRunWorkerConfig::default()
            },
        )
    };
    // Keep the capacity probe's retry outside the test's runtime. The claim
    // below scans every eligible run, so a short retry would let this first
    // run legitimately win again before the assertion on the second run.
    let capacity_worker = worker(RetrySchedule::new(
        Duration::from_secs(60 * 60),
        Duration::from_secs(60 * 60),
        Duration::from_secs(2 * 60 * 60),
    ));
    assert_eq!(
        capacity_worker.run_once().await.unwrap(),
        SandboxAgentRunWorkerOutcome::RetryScheduled(first)
    );
    let first_state = store.get_agent_run(first).await.unwrap().unwrap();
    assert_eq!(first_state.status, AgentRunStatus::RetryWait);
    assert!(first_state.lease_token.is_none());
    let second = child("second".into()).await;
    // The retry-wait lease is gone, so another queued child can claim the
    // only scheduler slot immediately.
    let claimed = store
        .claim_agent_run(uuid::Uuid::new_v4(), chrono::Duration::minutes(1), 1, 1)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed.id, second);
    store.request_agent_run_cancellation(second).await.unwrap();
    store
        .finish_agent_run_cancellation(second, claimed.lease_token.unwrap())
        .await
        .unwrap();

    let terminal = child("terminal".into()).await;
    let terminal_worker = worker(RetrySchedule::new(
        Duration::from_millis(100),
        Duration::from_millis(200),
        Duration::from_secs(60),
    ));
    assert_eq!(
        terminal_worker.run_once().await.unwrap(),
        SandboxAgentRunWorkerOutcome::RetryScheduled(terminal)
    );
    // Each remaining attempt parks in retry-wait until the durable
    // budget is spent and the run fails terminally.
    let mut outcome = SandboxAgentRunWorkerOutcome::RetryScheduled(terminal);
    for _ in 1..tidebreak_core::AgentRun::DEFAULT_MAX_ATTEMPTS {
        tokio::time::sleep(Duration::from_millis(300)).await;
        outcome = terminal_worker.run_once().await.unwrap();
    }
    assert_eq!(outcome, SandboxAgentRunWorkerOutcome::Failed(terminal));
    let terminal_state = store.get_agent_run(terminal).await.unwrap().unwrap();
    assert_eq!(terminal_state.status, AgentRunStatus::Failed);
    let inbox = store
        .list_agent_run_inbox(tidebreak_core::AgentRunId::foreground_for_chat(chat.id))
        .await
        .unwrap();
    assert!(inbox.iter().any(|entry| entry.child_run_id == terminal
        && entry.result.text.contains("sandbox_execution_failed")));
}

#[tokio::test]
async fn cancellation_while_resolving_prevents_the_provider_request() {
    let (_unused, store, provider, chat, _dir) = fixture().await;
    let call = CallId::new();
    let id = tidebreak_core::AgentRunId::sandbox_for_spawn_call(call);
    admit_sandbox(&store, chat.id, call, "do not call provider").await;
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let worker = SandboxAgentRunWorker::new(
        store.clone(),
        test_secrets(),
        Arc::new(DelayedResolver {
            entered: entered.clone(),
            release: release.clone(),
            provider: provider.clone(),
        }),
        Arc::new(Notify::new()),
        Arc::new(Notify::new()),
        Arc::new(EventBus::default()),
        AgentConfig {
            model: "m".into(),
            ..AgentConfig::default()
        },
        None,
        SandboxAgentRunWorkerConfig {
            heartbeat: Duration::from_millis(10),
            ..SandboxAgentRunWorkerConfig::default()
        },
    );
    let entered_wait = entered.notified();
    let execution = tokio::spawn(async move { worker.run_once().await });
    entered_wait.await;
    store.request_agent_run_cancellation(id).await.unwrap();
    release.notify_one();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), execution)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        SandboxAgentRunWorkerOutcome::Cancelled(id)
    );
    assert!(provider.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn deadline_clamped_noop_heartbeat_keeps_the_exact_in_process_lease_live() {
    let (_unused, store, provider, chat, dir) = fixture().await;
    let call = CallId::new();
    let id = tidebreak_core::AgentRunId::sandbox_for_spawn_call(call);
    admit_sandbox(&store, chat.id, call, "finish despite a clamped lease").await;

    // The claim is capped at the run's absolute deadline. Every heartbeat asks
    // for the same cap and therefore changes no row, even though the exact
    // claim remains live. A no-op renewal must be validated rather than
    // misclassified as lease loss before provider egress.
    let worker = SandboxAgentRunWorker::new(
        store.clone(),
        test_secrets(),
        Arc::new(FixedResolver(provider.clone())),
        Arc::new(Notify::new()),
        Arc::new(Notify::new()),
        Arc::new(EventBus::default()),
        AgentConfig {
            model: "m".into(),
            ..AgentConfig::default()
        },
        Some(dir.path().join("scratch-clamped-lease")),
        SandboxAgentRunWorkerConfig {
            lease: Duration::from_secs(24 * 60 * 60),
            heartbeat: Duration::from_millis(10),
            ..SandboxAgentRunWorkerConfig::default()
        },
    );

    assert_eq!(
        worker.run_once().await.unwrap(),
        SandboxAgentRunWorkerOutcome::Completed(id)
    );
    assert_eq!(provider.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn pre_signalled_cancellation_after_egress_heartbeat_never_polls_provider() {
    let (_unused, store, _provider, chat, _dir) = fixture().await;
    let call = CallId::new();
    let id = tidebreak_core::AgentRunId::sandbox_for_spawn_call(call);
    admit_sandbox(
        &store,
        chat.id,
        call,
        "cancel at the provider authorization fence",
    )
    .await;
    let lease_token = uuid::Uuid::new_v4();
    let claimed = store
        .claim_agent_run(lease_token, chrono::Duration::seconds(1), 1, 1)
        .await
        .unwrap()
        .expect("sandbox run should claim");
    assert_eq!(claimed.id, id);

    let provider_entries = Arc::new(AtomicUsize::new(0));
    let attempts = Arc::new(SandboxAttemptGuard::default());
    let worker = SandboxAgentRunWorker::with_attempts(
        store.clone(),
        test_secrets(),
        Arc::new(FixedResolver(Arc::new(CountingPendingProvider {
            entries: provider_entries.clone(),
        }))),
        Arc::new(Notify::new()),
        Arc::new(Notify::new()),
        Arc::new(EventBus::default()),
        attempts.clone(),
        AgentConfig {
            model: "m".into(),
            ..AgentConfig::default()
        },
        None,
        None,
        SandboxAgentRunWorkerConfig::default(),
    );
    let cancellation_store = store.clone();
    let cancellation_attempts = attempts.clone();
    let outcome = worker
        .process_after_pre_egress(claimed, lease_token, async move {
            assert!(matches!(
                cancellation_store
                    .request_agent_run_cancellation(id)
                    .await
                    .unwrap(),
                Some(tidebreak_core::RequestAgentRunCancellationOutcome::Requested(_))
            ));
            let signal = cancellation_store
                .get_agent_run_cancellation_signal(id)
                .await
                .unwrap()
                .expect("cancellation receipt should retain the claimed lease");
            assert_eq!(signal.lease_token, lease_token);
            assert!(cancellation_attempts.cancel_model(id, lease_token));
        })
        .await
        .unwrap();

    assert_eq!(outcome, SandboxAgentRunWorkerOutcome::Cancelled(id));
    assert_eq!(provider_entries.load(Ordering::SeqCst), 0);
    let run = store.get_agent_run(id).await.unwrap().unwrap();
    assert_eq!(run.status, AgentRunStatus::Cancelled);
    assert_eq!(run.model_steps, 0);
    assert_eq!(run.usage, Usage::default());
    let result = store.get_agent_run_result(id).await.unwrap().unwrap();
    assert!(matches!(
        result.payload,
        tidebreak_core::AgentRunResultPayload::Cancelled { .. }
    ));
    assert_eq!(result.model_steps, 0);
    assert_eq!(result.usage, Usage::default());
}

#[tokio::test]
async fn local_signal_drops_resolver_before_durable_cancellation_ack() {
    let (_unused, store, _provider, chat, _dir) = fixture().await;
    let call = CallId::new();
    let id = tidebreak_core::AgentRunId::sandbox_for_spawn_call(call);
    admit_sandbox(&store, chat.id, call, "cancel resolver").await;
    let entered = Arc::new(Notify::new());
    let dropped = Arc::new(AtomicBool::new(false));
    let attempts = Arc::new(SandboxAttemptGuard::default());
    let worker = SandboxAgentRunWorker::with_attempts(
        store.clone(),
        test_secrets(),
        Arc::new(DropAwareResolver {
            entered: entered.clone(),
            dropped: dropped.clone(),
        }),
        Arc::new(Notify::new()),
        Arc::new(Notify::new()),
        Arc::new(EventBus::default()),
        attempts.clone(),
        AgentConfig {
            model: "m".into(),
            ..AgentConfig::default()
        },
        None,
        None,
        SandboxAgentRunWorkerConfig::default(),
    );
    let entered_wait = entered.notified();
    let execution = tokio::spawn(async move { worker.run_once().await });
    entered_wait.await;
    assert!(matches!(
        store.request_agent_run_cancellation(id).await.unwrap(),
        Some(tidebreak_core::RequestAgentRunCancellationOutcome::Requested(_))
    ));
    let signal = store
        .get_agent_run_cancellation_signal(id)
        .await
        .unwrap()
        .unwrap();
    assert!(attempts.cancel_model(id, signal.lease_token));
    assert_eq!(
        execution.await.unwrap().unwrap(),
        SandboxAgentRunWorkerOutcome::Cancelled(id)
    );
    assert!(dropped.load(Ordering::SeqCst));
    assert_eq!(
        store.get_agent_run(id).await.unwrap().unwrap().status,
        AgentRunStatus::Cancelled
    );
}

#[tokio::test]
async fn local_signal_drops_provider_stream_before_durable_cancellation_ack() {
    let (_unused, store, _provider, chat, _dir) = fixture().await;
    let call = CallId::new();
    let id = tidebreak_core::AgentRunId::sandbox_for_spawn_call(call);
    admit_sandbox(&store, chat.id, call, "cancel completion").await;
    let started = Arc::new(Notify::new());
    let dropped = Arc::new(AtomicBool::new(false));
    let attempts = Arc::new(SandboxAttemptGuard::default());
    let worker = SandboxAgentRunWorker::with_attempts(
        store.clone(),
        test_secrets(),
        Arc::new(FixedResolver(Arc::new(DropAwareProvider {
            started: started.clone(),
            dropped: dropped.clone(),
            first_event: None,
        }))),
        Arc::new(Notify::new()),
        Arc::new(Notify::new()),
        Arc::new(EventBus::default()),
        attempts.clone(),
        AgentConfig {
            model: "m".into(),
            ..AgentConfig::default()
        },
        None,
        None,
        SandboxAgentRunWorkerConfig::default(),
    );
    let started_wait = started.notified();
    let execution = tokio::spawn(async move { worker.run_once().await });
    started_wait.await;
    assert!(matches!(
        store.request_agent_run_cancellation(id).await.unwrap(),
        Some(tidebreak_core::RequestAgentRunCancellationOutcome::Requested(_))
    ));
    let signal = store
        .get_agent_run_cancellation_signal(id)
        .await
        .unwrap()
        .unwrap();
    assert!(attempts.cancel_model(id, signal.lease_token));
    assert_eq!(
        execution.await.unwrap().unwrap(),
        SandboxAgentRunWorkerOutcome::Cancelled(id)
    );
    assert!(dropped.load(Ordering::SeqCst));
    assert_eq!(
        store.get_agent_run(id).await.unwrap().unwrap().status,
        AgentRunStatus::Cancelled
    );
}

#[tokio::test]
async fn local_cancellation_accounts_usage_observed_before_stream_drop() {
    let (_unused, store, _provider, chat, _dir) = fixture().await;
    let call = CallId::new();
    let id = tidebreak_core::AgentRunId::sandbox_for_spawn_call(call);
    admit_sandbox(&store, chat.id, call, "cancel after usage").await;
    let started = Arc::new(Notify::new());
    let dropped = Arc::new(AtomicBool::new(false));
    let attempts = Arc::new(SandboxAttemptGuard::default());
    let usage = Usage {
        input_tokens: 13,
        output_tokens: 5,
        ..Usage::default()
    };
    let worker = SandboxAgentRunWorker::with_attempts(
        store.clone(),
        test_secrets(),
        Arc::new(FixedResolver(Arc::new(DropAwareProvider {
            started: started.clone(),
            dropped: dropped.clone(),
            first_event: Some(ProviderEvent::Usage(usage)),
        }))),
        Arc::new(Notify::new()),
        Arc::new(Notify::new()),
        Arc::new(EventBus::default()),
        attempts.clone(),
        AgentConfig {
            model: "m".into(),
            ..AgentConfig::default()
        },
        None,
        None,
        SandboxAgentRunWorkerConfig::default(),
    );
    let started_wait = started.notified();
    let execution = tokio::spawn(async move { worker.run_once().await });
    started_wait.await;
    assert!(matches!(
        store.request_agent_run_cancellation(id).await.unwrap(),
        Some(tidebreak_core::RequestAgentRunCancellationOutcome::Requested(_))
    ));
    let signal = store
        .get_agent_run_cancellation_signal(id)
        .await
        .unwrap()
        .unwrap();
    assert!(attempts.cancel_model(id, signal.lease_token));
    assert_eq!(
        execution.await.unwrap().unwrap(),
        SandboxAgentRunWorkerOutcome::Cancelled(id)
    );
    assert!(dropped.load(Ordering::SeqCst));

    let run = store.get_agent_run(id).await.unwrap().unwrap();
    assert_eq!(run.status, AgentRunStatus::Cancelled);
    assert_eq!(run.model_steps, 1);
    assert_eq!(run.usage, usage);
    let result = store.get_agent_run_result(id).await.unwrap().unwrap();
    assert_eq!(result.model_steps, 1);
    assert_eq!(result.usage, usage);
}

/// Final accounting can remain unavailable beyond the execution lease that
/// was live when cancellation began. The immutable cancellation identity must
/// renew finalization-only authority, retry the same accounting CAS, and only
/// then snapshot the totals into the cancelled receipt.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_cancellation_finalization_survives_accounting_failure_past_execution_lease() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                dir.path().join("t.db").display()
            ))
            .await
            .unwrap(),
        );
        let chat = sandbox_chat();
        store.create_chat(&chat).await.unwrap();
        let call = CallId::new();
        let id = tidebreak_core::AgentRunId::sandbox_for_spawn_call(call);
        admit_sandbox(&store, chat.id, call, "cancel after delayed accounting").await;

        let started = Arc::new(Notify::new());
        let dropped = Arc::new(AtomicBool::new(false));
        let attempts = Arc::new(SandboxAttemptGuard::default());
        let usage = Usage {
            input_tokens: 19,
            output_tokens: 11,
            cache_read_input_tokens: 7,
            cache_creation_input_tokens: 3,
        };
        let lease = Duration::from_millis(150);
        let worker = SandboxAgentRunWorker::with_attempts(
            store.clone(),
            test_secrets(),
            Arc::new(FixedResolver(Arc::new(DropAwareProvider {
                started: started.clone(),
                dropped: dropped.clone(),
                first_event: Some(ProviderEvent::Usage(usage)),
            }))),
            Arc::new(Notify::new()),
            Arc::new(Notify::new()),
            Arc::new(EventBus::default()),
            attempts.clone(),
            AgentConfig {
                model: "m".into(),
                ..AgentConfig::default()
            },
            None,
            None,
            SandboxAgentRunWorkerConfig {
                lease,
                heartbeat: Duration::from_millis(40),
                ..SandboxAgentRunWorkerConfig::default()
            },
        );
        worker.fail_cancellation_accounting_until_released();
        let accounting_failure_observed = worker.cancellation_accounting_failure_observed.clone();
        let first_failure = accounting_failure_observed.notified();
        tokio::pin!(first_failure);
        let finalization_control = worker.clone();
        let started_wait = started.notified();
        let execution = tokio::spawn(async move { worker.run_once().await });

        started_wait.await;
        assert!(matches!(
            store.request_agent_run_cancellation(id).await.unwrap(),
            Some(tidebreak_core::RequestAgentRunCancellationOutcome::Requested(_))
        ));
        let cancelling = store.get_agent_run(id).await.unwrap().unwrap();
        let original_expiry = cancelling
            .lease_expires_at
            .expect("the cancelling run retains its execution lease expiry");
        let signal = store
            .get_agent_run_cancellation_signal(id)
            .await
            .unwrap()
            .unwrap();
        assert!(attempts.cancel_model(id, signal.lease_token));

        tokio::time::timeout(Duration::from_secs(2), first_failure)
            .await
            .expect("post-quiescence accounting reaches the injected outage");
        assert!(dropped.load(Ordering::SeqCst));
        let until_original_expiry = original_expiry
            .signed_duration_since(Utc::now())
            .to_std()
            .unwrap_or_default();
        tokio::time::sleep(until_original_expiry + Duration::from_millis(75)).await;
        assert!(Utc::now() > original_expiry);
        let still_cancelling = store.get_agent_run(id).await.unwrap().unwrap();
        assert_eq!(still_cancelling.status, AgentRunStatus::Cancelling);
        assert!(still_cancelling
            .lease_expires_at
            .is_some_and(|expiry| expiry > Utc::now()));

        finalization_control.release_cancellation_accounting();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), execution)
                .await
                .expect("finalization resumes after accounting recovers")
                .unwrap()
                .unwrap(),
            SandboxAgentRunWorkerOutcome::Cancelled(id)
        );
        assert!(
            finalization_control
                .cancellation_accounting_calls
                .load(Ordering::SeqCst)
                >= 2
        );

        let cancelled = store.get_agent_run(id).await.unwrap().unwrap();
        assert_eq!(cancelled.status, AgentRunStatus::Cancelled);
        assert_eq!(cancelled.model_steps, 1);
        assert_eq!(cancelled.usage, usage);
        let result = store.get_agent_run_result(id).await.unwrap().unwrap();
        assert!(matches!(
            &result.payload,
            tidebreak_core::AgentRunResultPayload::Cancelled { .. }
        ));
        assert_eq!(result.model_steps, 1);
        assert_eq!(result.usage, usage);
    })
    .await
    .expect("test completed within its time bound");
}

#[tokio::test]
async fn local_cancellation_accounts_output_observed_without_usage() {
    let (_unused, store, _provider, chat, _dir) = fixture().await;
    let call = CallId::new();
    let id = tidebreak_core::AgentRunId::sandbox_for_spawn_call(call);
    admit_sandbox(&store, chat.id, call, "cancel after output").await;
    let started = Arc::new(Notify::new());
    let dropped = Arc::new(AtomicBool::new(false));
    let attempts = Arc::new(SandboxAttemptGuard::default());
    let worker = SandboxAgentRunWorker::with_attempts(
        store.clone(),
        test_secrets(),
        Arc::new(FixedResolver(Arc::new(DropAwareProvider {
            started: started.clone(),
            dropped: dropped.clone(),
            first_event: Some(ProviderEvent::TextDelta {
                text: "partial".into(),
            }),
        }))),
        Arc::new(Notify::new()),
        Arc::new(Notify::new()),
        Arc::new(EventBus::default()),
        attempts.clone(),
        AgentConfig {
            model: "m".into(),
            ..AgentConfig::default()
        },
        None,
        None,
        SandboxAgentRunWorkerConfig::default(),
    );
    let started_wait = started.notified();
    let execution = tokio::spawn(async move { worker.run_once().await });
    started_wait.await;
    assert!(matches!(
        store.request_agent_run_cancellation(id).await.unwrap(),
        Some(tidebreak_core::RequestAgentRunCancellationOutcome::Requested(_))
    ));
    let signal = store
        .get_agent_run_cancellation_signal(id)
        .await
        .unwrap()
        .unwrap();
    assert!(attempts.cancel_model(id, signal.lease_token));
    assert_eq!(
        execution.await.unwrap().unwrap(),
        SandboxAgentRunWorkerOutcome::Cancelled(id)
    );
    assert!(dropped.load(Ordering::SeqCst));

    let run = store.get_agent_run(id).await.unwrap().unwrap();
    assert_eq!(run.status, AgentRunStatus::Cancelled);
    assert_eq!(run.model_steps, 1);
    assert_eq!(run.usage, Usage::default());
    let result = store.get_agent_run_result(id).await.unwrap().unwrap();
    assert_eq!(result.model_steps, 1);
    assert_eq!(result.usage, Usage::default());
}

#[tokio::test]
async fn cancellation_before_local_registration_is_closed_by_first_heartbeat() {
    let (_unused, store, provider, chat, _dir) = fixture().await;
    let call = CallId::new();
    let id = tidebreak_core::AgentRunId::sandbox_for_spawn_call(call);
    admit_sandbox(&store, chat.id, call, "cancel before register").await;
    let claimed = store
        .claim_agent_run(uuid::Uuid::new_v4(), chrono::Duration::minutes(5), 1, 1)
        .await
        .unwrap()
        .unwrap();
    let lease = claimed.lease_token.unwrap();
    assert!(matches!(
        store.request_agent_run_cancellation(id).await.unwrap(),
        Some(tidebreak_core::RequestAgentRunCancellationOutcome::Requested(_))
    ));
    let worker = SandboxAgentRunWorker::new(
        store.clone(),
        test_secrets(),
        Arc::new(FixedResolver(provider.clone())),
        Arc::new(Notify::new()),
        Arc::new(Notify::new()),
        Arc::new(EventBus::default()),
        AgentConfig::default(),
        None,
        SandboxAgentRunWorkerConfig::default(),
    );
    assert_eq!(
        worker.process(claimed, lease).await.unwrap(),
        SandboxAgentRunWorkerOutcome::Cancelled(id)
    );
    assert!(provider.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn resolver_delay_past_the_database_lease_prevents_provider_egress() {
    let (_unused, store, provider, chat, _dir) = fixture().await;
    let call = CallId::new();
    admit_sandbox(&store, chat.id, call, "do not call provider after expiry").await;
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let worker = SandboxAgentRunWorker::new(
        store,
        test_secrets(),
        Arc::new(DelayedResolver {
            entered: entered.clone(),
            release: release.clone(),
            provider: provider.clone(),
        }),
        Arc::new(Notify::new()),
        Arc::new(Notify::new()),
        Arc::new(EventBus::default()),
        AgentConfig {
            model: "m".into(),
            ..AgentConfig::default()
        },
        None,
        SandboxAgentRunWorkerConfig {
            lease: Duration::from_secs(1),
            heartbeat: Duration::from_millis(5),
            suppress_resolver_heartbeats: true,
            ..SandboxAgentRunWorkerConfig::default()
        },
    );
    let entered_wait = entered.notified();
    let execution = tokio::spawn(async move { worker.run_once().await });
    entered_wait.await;
    // With periodic resolver heartbeats deliberately held for this test,
    // this expiry is fenced by the final DB-clock heartbeat immediately
    // before `provider.stream`.
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    release.notify_one();
    let outcome = tokio::time::timeout(Duration::from_secs(1), execution)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(
        matches!(outcome, SandboxAgentRunWorkerOutcome::LeaseLost(_)),
        "{outcome:?}"
    );
    assert!(provider.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn sandbox_request_does_not_inherit_foreground_system_or_tools() {
    let dir = tempfile::tempdir().unwrap();
    let store = DbStore::connect(&format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("t.db").display()
    ))
    .await
    .unwrap();
    let request = sandbox_request(
        &AgentConfig {
            model: "m".into(),
            system_prompt: Some("foreground only".into()),
            ..AgentConfig::default()
        },
        "task".into(),
        &[],
        &store,
        false,
        &[],
        &[],
    )
    .await
    .unwrap();
    assert_eq!(
        request.system.as_deref(),
        Some(sandbox_system_prompt(false, TurnWebSearch::Host, true, &[], &[]).as_str())
    );
    assert_eq!(
        request
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        vec![
            SANDBOX_EXEC_TOOL,
            tidebreak_core::SANDBOX_WEB_SEARCH_TOOL,
            tidebreak_core::UPDATE_TASK_PLAN_TOOL,
            tidebreak_core::SANDBOX_DONE_TOOL,
            tidebreak_core::REQUEST_FOLDER_ACCESS_TOOL,
        ]
    );
}

#[tokio::test]
async fn sandbox_request_withholds_tools_from_a_chat_only_model() {
    let dir = tempfile::tempdir().unwrap();
    let store = DbStore::connect(&format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("t.db").display()
    ))
    .await
    .unwrap();
    let request = sandbox_request(
        &AgentConfig {
            model: "chat-only".into(),
            tools_supported: false,
            web_search: TurnWebSearch::Vendor(tidebreak_core::VendorWebSearch {
                max_uses: tidebreak_core::VendorWebSearch::DEFAULT_MAX_USES,
            }),
            ..AgentConfig::default()
        },
        "task".into(),
        &[],
        &store,
        false,
        &[],
        &[],
    )
    .await
    .unwrap();
    assert!(request.tools.is_empty());
    assert!(request.vendor_web_search.is_none());
    let prompt = request.system.as_deref().unwrap();
    assert_eq!(
        prompt,
        sandbox_system_prompt(
            false,
            TurnWebSearch::Vendor(tidebreak_core::VendorWebSearch {
                max_uses: tidebreak_core::VendorWebSearch::DEFAULT_MAX_USES,
            }),
            false,
            &[],
            &[],
        )
    );
    for unavailable in [
        "exec",
        "search",
        "delegat",
        "done",
        "update_task_plan",
        "request_folder_access",
    ] {
        assert!(
            !prompt.contains(unavailable),
            "chat-only sandbox prompt advertised unavailable capability `{unavailable}`: {prompt}"
        );
    }
    assert!(prompt.contains("Return the best final text result directly"));
}

#[tokio::test]
async fn one_model_step_never_advertises_unconsumable_web_search_work() {
    let dir = tempfile::tempdir().unwrap();
    let store = DbStore::connect(&format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("t.db").display()
    ))
    .await
    .unwrap();
    let request = sandbox_request(
        &AgentConfig {
            model: "m".into(),
            max_steps: 1,
            web_search: TurnWebSearch::Vendor(tidebreak_core::VendorWebSearch {
                max_uses: tidebreak_core::VendorWebSearch::DEFAULT_MAX_USES,
            }),
            ..AgentConfig::default()
        },
        "task".into(),
        &[],
        &store,
        false,
        &[],
        &[],
    )
    .await
    .unwrap();
    assert!(request.vendor_web_search.is_none());
    assert!(request
        .system
        .as_deref()
        .is_some_and(|prompt| !prompt.contains(SANDBOX_PROMPT_WEB_SEARCH_CLAUSE)));
    assert_eq!(
        request
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        vec![
            tidebreak_core::SANDBOX_DONE_TOOL,
            tidebreak_core::REQUEST_FOLDER_ACCESS_TOOL,
        ]
    );
    let provider = Arc::new(EventProvider(vec![
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
    ]));
    // Calling a withdrawn tool is answered rather than failed: the step
    // budget is spent, so the run needs the correction in its transcript,
    // not a burned attempt.
    let completion = complete_sandbox_task(provider, request)
        .await
        .outcome
        .unwrap()
        .completion;
    let SandboxCompletion::ToolCalls(intents) = completion else {
        panic!("a withdrawn tool must still produce a tool call");
    };
    let [intent] = <[_; 1]>::try_from(intents).expect("one call");
    assert!(matches!(
        intent.disposition,
        SandboxToolCallDisposition::Rejected {
            error_code: "unavailable_tool",
            ..
        }
    ));
}

#[tokio::test]
async fn desktop_delegation_advertises_one_canonical_file_read() {
    let dir = tempfile::tempdir().unwrap();
    let store = DbStore::connect(&format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("t.db").display()
    ))
    .await
    .unwrap();
    let request = sandbox_request(
        &AgentConfig {
            model: "m".into(),
            ..AgentConfig::default()
        },
        "task".into(),
        &[],
        &store,
        true,
        &[],
        &[],
    )
    .await
    .unwrap();
    assert_eq!(
        request.system.as_deref(),
        Some(sandbox_system_prompt(true, TurnWebSearch::Host, true, &[], &[]).as_str())
    );
    assert_eq!(
        request
            .tools
            .iter()
            .filter(|tool| tool.name == tidebreak_core::SANDBOX_READ_DELEGATED_FILE_TOOL)
            .count(),
        1
    );

    let provider = Arc::new(EventProvider(vec![
        ProviderEvent::ToolCallStarted {
            index: 0,
            id: "read_1".into(),
            name: tidebreak_core::SANDBOX_READ_DELEGATED_FILE_TOOL.into(),
        },
        ProviderEvent::ToolCallArgsDelta {
            index: 0,
            fragment: "{}".into(),
        },
        ProviderEvent::Stop {
            reason: StopReason::ToolUse,
        },
    ]));
    assert!(matches!(
        complete_sandbox_task(provider, request)
            .await
            .outcome
            .unwrap()
            .completion,
        SandboxCompletion::ToolCalls(ref intents)
            if matches!(
                intents.as_slice(),
                [SandboxToolCallIntent {
                    name,
                    arguments,
                    disposition: SandboxToolCallDisposition::Execute,
                    ..
                }] if name == tidebreak_core::SANDBOX_READ_DELEGATED_FILE_TOOL
                    && *arguments == serde_json::json!({})
            )
    ));
}

#[tokio::test]
async fn delegated_file_read_answers_nonempty_arguments_without_parking_work() {
    let request = ChatRequest {
        provider: None,
        model: "m".into(),
        reasoning_model: false,
        system: Some(sandbox_system_prompt(
            true,
            TurnWebSearch::Host,
            true,
            &[],
            &[],
        )),
        messages: vec![ChatMessage::text(Role::User, "task")],
        tools: vec![sandbox_read_delegated_file_tool_spec()],
        max_tokens: Some(100),
        temperature: Some(0.0),
        reasoning_effort: None,
        images: tidebreak_core::ImageAttachments::new(),
        ..Default::default()
    };
    let provider = Arc::new(EventProvider(vec![
        ProviderEvent::ToolCallStarted {
            index: 0,
            id: "read_1".into(),
            name: tidebreak_core::SANDBOX_READ_DELEGATED_FILE_TOOL.into(),
        },
        ProviderEvent::ToolCallArgsDelta {
            index: 0,
            fragment: r#"{"path":"secret"}"#.into(),
        },
        ProviderEvent::Stop {
            reason: StopReason::ToolUse,
        },
    ]));
    let completion = complete_sandbox_task(provider, request)
        .await
        .outcome
        .unwrap()
        .completion;
    let SandboxCompletion::ToolCalls(intents) = completion else {
        panic!("invalid delegated-file arguments must stay a tool call");
    };
    let [intent] = <[_; 1]>::try_from(intents).expect("one call");
    assert_eq!(intent.arguments, serde_json::json!({"path":"secret"}));
    assert!(
        matches!(
            intent.disposition,
            SandboxToolCallDisposition::Rejected {
                error_code: "invalid_arguments",
                ..
            }
        ),
        "{:?}",
        intent.disposition
    );
}

#[tokio::test]
async fn delegated_file_advertisement_requires_exact_current_attachment() {
    let (_worker, store, _provider, mut chat, _dir) = fixture().await;
    let run = admit_sandbox(&store, chat.id, CallId::new(), "inspect file").await;
    let mut admission = store
        .get_sandbox_agent_admission(run.id)
        .await
        .unwrap()
        .unwrap();
    assert!(!delegated_file_admission_matches(&run, &admission, &chat));

    let root_id = tidebreak_core::HostRootId::from_uuid(uuid::Uuid::new_v4()).unwrap();
    admission.resource = Some(tidebreak_core::SandboxAgentFileResource {
        root_id,
        relative_path: "reports/summary.md".into(),
    });
    assert!(!delegated_file_admission_matches(&run, &admission, &chat));
    chat.root_attachments
        .push(tidebreak_core::ChatRootAttachment {
            root_id,
            origin: tidebreak_core::RootAttachmentOrigin::Conversation,
        });
    assert!(delegated_file_admission_matches(&run, &admission, &chat));

    admission.chat_id = ChatId::new();
    assert!(!delegated_file_admission_matches(&run, &admission, &chat));
}

#[tokio::test]
async fn desktop_worker_checkpoints_one_exact_delegated_file_read() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let root_id = tidebreak_core::HostRootId::from_uuid(uuid::Uuid::new_v4()).unwrap();
    let mut chat = sandbox_chat();
    chat.attachment_revision = 1;
    chat.root_attachments
        .push(tidebreak_core::ChatRootAttachment {
            root_id,
            origin: tidebreak_core::RootAttachmentOrigin::Conversation,
        });
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    store
        .accept_turn(turn_id, chat.id, "model", "spawn delegated child")
        .await
        .unwrap();
    let foreground_lease = uuid::Uuid::new_v4();
    let now = Utc::now();
    let turn = store
        .claim_turn_run(foreground_lease, now, now + chrono::Duration::minutes(1))
        .await
        .unwrap()
        .turn
        .unwrap();
    store
        .append_turn_event(
            chat.id,
            turn.id,
            foreground_lease,
            1,
            Utc::now(),
            &tidebreak_core::AgentEvent::TurnStarted { turn_id: turn.id },
        )
        .await
        .unwrap();
    let spawn_call_id = CallId::new();
    let child_id = tidebreak_core::AgentRunId::sandbox_for_spawn_call(spawn_call_id);
    let outcome = store
        .checkpoint_sandbox_spawn(
            &tidebreak_core::SandboxSpawnCheckpointRequest {
                origin_turn_id: turn.id,
                lease_token: foreground_lease,
                expected_steer_revision: turn.steer_revision,
                call_id: spawn_call_id,
                provider_id: "spawn_1".into(),
                arguments: serde_json::json!({
                    "task": "inspect the report",
                    "resource": {
                        "root_id": root_id,
                        "relative_path": "reports/summary.md"
                    }
                }),
                approval_gated: false,
                result: serde_json::to_string(&tidebreak_core::SpawnSandboxAgentResult {
                    agent_id: child_id,
                })
                .unwrap(),
                event_ordinal: 2,
                progress: TurnCheckpointProgress {
                    model_steps: 1,
                    usage: Usage::default(),
                },
                remaining_requests: Vec::new(),
                max_active_background_agents:
                    tidebreak_core::AgentRun::DEFAULT_MAX_ACTIVE_BACKGROUND_AGENTS,
                execution_location: tidebreak_core::AgentRunExecutionLocation::InProcess,
            },
            Utc::now(),
        )
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        outcome,
        tidebreak_core::CheckpointSandboxSpawnOutcome::Checkpointed { .. }
            | tidebreak_core::CheckpointSandboxSpawnOutcome::Existing { .. }
    ));

    let provider = Arc::new(EventProvider(vec![
        ProviderEvent::ToolCallStarted {
            index: 0,
            id: "read_1".into(),
            name: tidebreak_core::SANDBOX_READ_DELEGATED_FILE_TOOL.into(),
        },
        ProviderEvent::ToolCallArgsDelta {
            index: 0,
            fragment: "{}".into(),
        },
        ProviderEvent::Stop {
            reason: StopReason::ToolUse,
        },
    ]));
    let worker = SandboxAgentRunWorker::new(
        store.clone(),
        test_secrets(),
        Arc::new(FixedResolver(provider)),
        Arc::new(Notify::new()),
        Arc::new(Notify::new()),
        Arc::new(EventBus::default()),
        AgentConfig {
            model: "model".into(),
            ..AgentConfig::default()
        },
        Some(dir.path().join("scratch")),
        SandboxAgentRunWorkerConfig::default().with_delegated_file_executor(true),
    );
    assert!(matches!(
        worker.run_once().await.unwrap(),
        SandboxAgentRunWorkerOutcome::ToolCheckpointed(_)
    ));
    let calls = store
        .list_sandbox_tool_calls_for_agent_run(child_id)
        .await
        .unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].name,
        tidebreak_core::SANDBOX_READ_DELEGATED_FILE_TOOL
    );
    assert_eq!(calls[0].arguments, serde_json::json!({}));
}

/// Production routing resolves models through the host registry, which is
/// what lets a run claim the vendor search its model's adapter emits.
struct RegistryResolver(Arc<dyn ModelProvider>);

#[async_trait]
impl ProviderResolver for RegistryResolver {
    async fn resolve(&self) -> Arc<dyn ModelProvider> {
        self.0.clone()
    }

    fn enforces_model_registry(&self) -> bool {
        true
    }
}

/// A provider that searches on its own infrastructure and then answers,
/// which is the whole shape of a vendor step: the search is reported as
/// already finished, and the run never sees a tool call to make.
#[derive(Default)]
struct VendorSearchProvider {
    requests: Mutex<Vec<ChatRequest>>,
}

#[async_trait]
impl ModelProvider for VendorSearchProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("vendor-search")
    }

    async fn stream(&self, request: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        self.requests.lock().unwrap().push(request);
        Ok(stream::iter(vec![
            ProviderEvent::ProviderExecutedToolCall {
                name: tidebreak_core::SANDBOX_WEB_SEARCH_TOOL.into(),
                input: serde_json::json!({"query": "Tidebreak release notes"}),
                output: serde_json::json!({"provider": "vendor-search", "results": []}),
                is_error: false,
                replay: None,
            },
            ProviderEvent::TextDelta {
                text: "Nothing new was published.".into(),
            },
            ProviderEvent::Stop {
                reason: StopReason::EndTurn,
            },
        ])
        .boxed())
    }
}

async fn set_web_search_mode(store: &Arc<dyn Store>, mode: &str) {
    store
        .set_setting(
            "web_search",
            &serde_json::json!({ "mode": mode, "timeout_ms": 20_000 }),
        )
        .await
        .unwrap();
}

/// Freeze the origin turn on an exact model so admission carries it to the
/// child, the way a real conversation's selection does.
async fn running_turn_on_model(store: &Arc<dyn Store>, chat_id: ChatId, model: &str) {
    let turn_id = TurnId::new();
    store
        .accept_turn(turn_id, chat_id, model, "delegate this")
        .await
        .unwrap();
    let now = Utc::now();
    store
        .claim_turn_run(uuid::Uuid::new_v4(), now, now + chrono::Duration::hours(1))
        .await
        .unwrap()
        .turn
        .expect("origin turn should claim");
}

/// A background run resolves web search from the one host setting, exactly
/// as a foreground turn does: the vendor route withholds the host tool the
/// run would otherwise checkpoint on, sets the request's budget, and keeps
/// the searches the provider ran in the run's own progress feed. Nothing
/// reaches the host web-search lane, because no checkpoint is parked for it.
#[tokio::test]
async fn a_vendor_run_searches_through_its_provider_and_parks_no_host_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let chat = sandbox_chat();
    store.create_chat(&chat).await.unwrap();
    set_web_search_mode(&store, "vendor").await;
    running_turn_on_model(&store, chat.id, "anthropic::claude-opus-5").await;
    let provider = Arc::new(VendorSearchProvider::default());
    let worker = SandboxAgentRunWorker::new(
        store.clone(),
        test_secrets(),
        Arc::new(RegistryResolver(provider.clone())),
        Arc::new(Notify::new()),
        Arc::new(Notify::new()),
        Arc::new(EventBus::default()),
        AgentConfig::default(),
        None,
        SandboxAgentRunWorkerConfig::default(),
    );
    let spawn = CallId::new();
    let id = tidebreak_core::AgentRunId::sandbox_for_spawn_call(spawn);
    admit_sandbox(&store, chat.id, spawn, "What shipped this week?").await;

    assert_eq!(
        worker.run_once().await.unwrap(),
        SandboxAgentRunWorkerOutcome::Completed(id)
    );
    let request = {
        let mut requests = provider.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        requests.pop().unwrap()
    };
    assert_eq!(
        request.vendor_web_search,
        Some(tidebreak_core::VendorWebSearch {
            max_uses: tidebreak_core::VendorWebSearch::DEFAULT_MAX_USES,
        })
    );
    assert!(!request
        .tools
        .iter()
        .any(|tool| tool.name == tidebreak_core::SANDBOX_WEB_SEARCH_TOOL));
    // The run still has a search and is still told about it; only which side
    // runs it changed.
    assert!(request
        .system
        .as_deref()
        .is_some_and(|system| system.contains(SANDBOX_PROMPT_WEB_SEARCH_CLAUSE)));
    assert!(store
        .list_sandbox_tool_calls_for_agent_run(id)
        .await
        .unwrap()
        .is_empty());
    assert!(store
        .list_sandbox_tool_call_candidates_named(tidebreak_core::SANDBOX_WEB_SEARCH_TOOL, 10)
        .await
        .unwrap()
        .is_empty());
    let progress = store.list_agent_run_progress(id, 0, 50).await.unwrap();
    assert_eq!(progress.len(), 1);
    assert_eq!(progress[0].text, "Ran web_search: Tidebreak release notes");
}

/// The vendor route is a claim about the model's adapter, so a run whose
/// model did not resolve through the registry has no vendor search to fall
/// back on — and the operator who chose that route asked for no host search
/// either. The run gets none, and its prompt stops naming one.
#[tokio::test]
async fn a_vendor_run_on_an_unregistered_model_gets_no_search_at_all() {
    let (worker, store, provider, chat, _dir) = fixture().await;
    set_web_search_mode(&store, "vendor").await;
    let spawn = CallId::new();
    let id = tidebreak_core::AgentRunId::sandbox_for_spawn_call(spawn);
    admit_sandbox(&store, chat.id, spawn, "What shipped this week?").await;

    assert_eq!(
        worker.run_once().await.unwrap(),
        SandboxAgentRunWorkerOutcome::Completed(id)
    );
    let requests = provider.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].vendor_web_search, None);
    assert!(!requests[0]
        .tools
        .iter()
        .any(|tool| tool.name == tidebreak_core::SANDBOX_WEB_SEARCH_TOOL));
    assert!(requests[0]
        .system
        .as_deref()
        .is_some_and(|system| !system.contains(SANDBOX_PROMPT_WEB_SEARCH_CLAUSE)));
}

/// Agents that stuff a whole shell line into `command` burn steps and fail
/// closed; the system prompt must name the argv contract up front.
#[test]
fn sandbox_system_prompt_teaches_exec_argv_form() {
    let prompt = sandbox_system_prompt(false, TurnWebSearch::Host, true, &[], &[]);
    assert!(
        prompt.contains(SANDBOX_PROMPT_EXEC_CLAUSE),
        "tool-capable runs must name the exec argv contract: {prompt}"
    );
    assert!(
        prompt.contains("single executable"),
        "prompt should spell out that command is argv[0] only: {prompt}"
    );
    assert!(
        prompt.contains("output/"),
        "prompt must still point deliverables at output/: {prompt}"
    );
    // Empty catalog: no skills section and no "there is no skills catalog"
    // denial that would contradict a later host with skills.
    assert!(!prompt.contains(SANDBOX_PROMPT_SKILLS_INTRO));
    assert!(!prompt.contains("no skills catalog"));
}

/// Tool-capable sandbox prompts carry the host skill catalog (names, one-line
/// descriptions, install pins) so children do not reinvent openpyxl/fpdf after
/// failed steps. Chat-only runs stay silent, and full SKILL.md bodies never
/// enter the prompt.
#[test]
fn tool_capable_sandbox_prompt_includes_host_skills_summary() {
    let skills = vec![
        tidebreak_code_execution::SkillPackage {
            name: "pdf-documents".into(),
            description: "Generate and manipulate PDF documents.".into(),
            python_deps: vec!["fpdf2==2.8.3".into(), "pypdf==6.15.0".into()],
            npm_deps: Vec::new(),
            host_deps: Vec::new(),
            origin: tidebreak_code_execution::SkillOrigin::Builtin,
        },
        tidebreak_code_execution::SkillPackage {
            name: "spreadsheets".into(),
            description: "Build Excel workbooks with openpyxl.".into(),
            python_deps: vec!["openpyxl==3.1.5".into()],
            npm_deps: Vec::new(),
            host_deps: vec![tidebreak_code_execution::HostDep::LibreOffice],
            origin: tidebreak_code_execution::SkillOrigin::Builtin,
        },
        tidebreak_code_execution::SkillPackage {
            name: "presentations".into(),
            description: "Build PowerPoint decks.".into(),
            python_deps: Vec::new(),
            npm_deps: vec!["pptxgenjs@4.0.1".into()],
            host_deps: vec![tidebreak_code_execution::HostDep::LibreOffice],
            origin: tidebreak_code_execution::SkillOrigin::Builtin,
        },
    ];
    let plugins = vec![tidebreak_code_execution::PluginPackage {
        name: "documents".into(),
        display_name: "Documents".into(),
        description: "Bundle.".into(),
        category: tidebreak_code_execution::PluginCategory::Documents,
        skills: vec!["pdf-documents".into(), "presentations".into()],
        prompts: Vec::new(),
        router_preamble: Some(
            "Pick by the file: pdf-documents for PDF, presentations for decks.".into(),
        ),
        mcp_servers: 0,
        origin: tidebreak_code_execution::PluginOrigin::Builtin,
        compatibility: tidebreak_code_execution::PluginCompatibility::compatible(),
    }];

    let summary = sandbox_skills_summary(&skills, &plugins).expect("catalog present");
    assert!(summary.contains(SANDBOX_PROMPT_SKILLS_INTRO));
    assert!(summary.contains("- pdf-documents: Generate and manipulate PDF documents."));
    assert!(summary.contains("pip install --user fpdf2==2.8.3 pypdf==6.15.0"));
    assert!(summary.contains("npm install --ignore-scripts pptxgenjs@4.0.1"));
    assert!(summary.contains("- spreadsheets: Build Excel workbooks with openpyxl."));
    assert!(summary.contains("- Pick by the file: pdf-documents for PDF, presentations for decks."));
    // Never paste skill bodies.
    assert!(!summary.contains("# PDF documents"));
    assert!(!summary.contains("Produce PDF deliverables"));

    let prompt = sandbox_system_prompt(false, TurnWebSearch::Host, true, &skills, &plugins);
    assert!(
        prompt.contains(SANDBOX_PROMPT_SKILLS_INTRO),
        "tool-capable prompt must include the skills summary: {prompt}"
    );
    assert!(prompt.contains("openpyxl==3.1.5"));
    assert!(prompt.contains(".tidebreak/skills/<name>/SKILL.md"));

    // Chat-only routes never advertise skills or exec install paths.
    let chat_only = sandbox_system_prompt(false, TurnWebSearch::Off, false, &skills, &plugins);
    assert_eq!(chat_only, super::config::SANDBOX_CHAT_ONLY_PROMPT);
    assert!(!chat_only.contains("pdf-documents"));
    assert!(!chat_only.contains(SANDBOX_PROMPT_SKILLS_INTRO));
}

fn sandbox_chat() -> Chat {
    Chat {
        id: ChatId::new(),
        project_id: None,
        title: Some("sandbox".into()),
        model: Some("model".into()),
        reasoning_effort: None,
        permission_mode: None,
        network_policy: Default::default(),
        attachment_revision: 0,
        root_attachments: Vec::new(),
        created_at: chrono::Utc::now(),
    }
}

/// Emits `plan_calls` successive `update_task_plan` calls, then `done` for
/// every step after that.
struct TaskPlanThenDoneProvider {
    requests: Mutex<Vec<ChatRequest>>,
    plan_calls: usize,
}

impl Default for TaskPlanThenDoneProvider {
    fn default() -> Self {
        Self {
            requests: Mutex::new(Vec::new()),
            plan_calls: 2,
        }
    }
}

impl TaskPlanThenDoneProvider {
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
impl ModelProvider for TaskPlanThenDoneProvider {
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
            // Every later step tries to finish. The first attempt may still
            // have an open step and be handed back; a later one is accepted
            // whether or not the model closed the plan.
            Self::done_call(&format!("done_{call_number}"))
        };
        Ok(stream::iter(events).boxed())
    }
}

/// A background run keeps a plan across its own checkpoints, and abandoning it
/// mid-list is pointed out once rather than accepted silently or refused.
///
/// This drives the whole path the way the run does: the tool is advertised to
/// the sandbox surface, each call parks a durable checkpoint that the host —
/// not the sandbox — resolves, and the plan that comes back is keyed by the
/// run. The `done` push-back is the part worth pinning: it must be one
/// rejected call the model can read and answer, and it must never be able to
/// stop the run from finishing.
#[tokio::test]
async fn a_sandbox_run_keeps_a_plan_and_is_reminded_once_before_it_finishes() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let chat = sandbox_chat();
    store.create_chat(&chat).await.unwrap();
    let provider = Arc::new(TaskPlanThenDoneProvider::default());
    let worker = SandboxAgentRunWorker::new(
        store.clone(),
        test_secrets(),
        Arc::new(FixedResolver(provider.clone())),
        Arc::new(Notify::new()),
        Arc::new(Notify::new()),
        Arc::new(EventBus::default()),
        AgentConfig {
            model: "sandbox-model".into(),
            max_steps: 8,
            ..AgentConfig::default()
        },
        None,
        SandboxAgentRunWorkerConfig::default(),
    );
    let plans = crate::sandbox_task_plan_worker::SandboxTaskPlanWorker::new(
        store.clone(),
        Arc::new(Notify::new()),
        crate::sandbox_task_plan_worker::SandboxTaskPlanWorkerConfig::default(),
    );
    let spawn = CallId::new();
    let id = tidebreak_core::AgentRunId::sandbox_for_spawn_call(spawn);
    admit_sandbox(&store, chat.id, spawn, "Research this.").await;

    // The plan tool is offered to the sandbox surface alongside the rest.
    assert!(matches!(
        worker.run_once().await.unwrap(),
        SandboxAgentRunWorkerOutcome::ToolCheckpointed(_)
    ));
    assert!(provider.requests.lock().unwrap()[0]
        .tools
        .iter()
        .any(|tool| tool.name == tidebreak_core::UPDATE_TASK_PLAN_TOOL));
    // Nothing is recorded until the host lane resolves the checkpoint: the row
    // lives in this database, not where the agent runs.
    assert!(store.get_agent_run_task_plan(id).await.unwrap().is_none());
    assert!(matches!(
        plans.run_once().await.unwrap(),
        crate::sandbox_task_plan_worker::SandboxTaskPlanWorkerOutcome::Resolved(_)
    ));
    let first = store.get_agent_run_task_plan(id).await.unwrap().unwrap();
    assert_eq!(first.run_id, id);
    assert_eq!(
        first.steps[0].status,
        tidebreak_core::TaskPlanStepStatus::InProgress
    );

    // The second call replaces the list rather than appending to it.
    assert!(matches!(
        worker.run_once().await.unwrap(),
        SandboxAgentRunWorkerOutcome::ToolCheckpointed(_)
    ));
    assert!(matches!(
        plans.run_once().await.unwrap(),
        crate::sandbox_task_plan_worker::SandboxTaskPlanWorkerOutcome::Resolved(_)
    ));
    let second = store.get_agent_run_task_plan(id).await.unwrap().unwrap();
    assert_eq!(second.steps.len(), 2);
    assert_eq!(
        second.steps[0].status,
        tidebreak_core::TaskPlanStepStatus::Completed
    );
    assert_eq!(
        second.steps[1].status,
        tidebreak_core::TaskPlanStepStatus::InProgress
    );

    // `done` with an unfinished plan is handed back as an ordinary error
    // result, keeping the run's attempt, rather than completing it.
    let reminder = match worker.run_once().await.unwrap() {
        SandboxAgentRunWorkerOutcome::ToolCheckpointed(call_id) => call_id,
        outcome => panic!("unexpected outcome: {outcome:?}"),
    };
    let receipt = store
        .get_sandbox_tool_call_receipt(reminder)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(receipt.status, SandboxToolCallStatus::Failed);
    assert_eq!(receipt.error_code.as_deref(), Some("task_plan_incomplete"));
    assert!(receipt.result.contains("write the summary"));
    assert_eq!(
        store.get_agent_run(id).await.unwrap().unwrap().status,
        AgentRunStatus::RetryWait
    );

    // The reminder is spent. A run that calls `done` again finishes, so a
    // model that will not close its list is never trapped.
    assert_eq!(
        worker.run_once().await.unwrap(),
        SandboxAgentRunWorkerOutcome::Completed(id)
    );
}

/// A run one model step from its ceiling submits instead of being reminded.
///
/// The reminder parks a row the next completion has to read, so it can only be
/// offered while the run can still pay for that completion. Getting this wrong
/// turns the nudge into a failure on exactly the runs that were about to
/// finish: the step budget would be exceeded assembling the request that was
/// supposed to carry the correction.
#[tokio::test]
async fn a_run_at_its_last_step_submits_rather_than_being_reminded() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let chat = sandbox_chat();
    store.create_chat(&chat).await.unwrap();
    let provider = Arc::new(TaskPlanThenDoneProvider::with_plan_calls(1));
    let worker = SandboxAgentRunWorker::new(
        store.clone(),
        test_secrets(),
        Arc::new(FixedResolver(provider.clone())),
        Arc::new(Notify::new()),
        Arc::new(Notify::new()),
        Arc::new(EventBus::default()),
        AgentConfig {
            model: "sandbox-model".into(),
            // One checkpoint, then one completion to read it. `done` on that
            // second step leaves no room for a third.
            max_steps: 2,
            ..AgentConfig::default()
        },
        None,
        SandboxAgentRunWorkerConfig::default(),
    );
    let plans = crate::sandbox_task_plan_worker::SandboxTaskPlanWorker::new(
        store.clone(),
        Arc::new(Notify::new()),
        crate::sandbox_task_plan_worker::SandboxTaskPlanWorkerConfig::default(),
    );
    let spawn = CallId::new();
    let id = tidebreak_core::AgentRunId::sandbox_for_spawn_call(spawn);
    admit_sandbox(&store, chat.id, spawn, "Research this.").await;

    assert!(matches!(
        worker.run_once().await.unwrap(),
        SandboxAgentRunWorkerOutcome::ToolCheckpointed(_)
    ));
    assert!(matches!(
        plans.run_once().await.unwrap(),
        crate::sandbox_task_plan_worker::SandboxTaskPlanWorkerOutcome::Resolved(_)
    ));
    // The plan is left with an open step, which is exactly what would earn a
    // reminder if the run could afford one.
    assert!(!tidebreak_core::open_task_plan_steps(
        &store
            .get_agent_run_task_plan(id)
            .await
            .unwrap()
            .unwrap()
            .steps
    )
    .is_empty());
    assert_eq!(
        worker.run_once().await.unwrap(),
        SandboxAgentRunWorkerOutcome::Completed(id)
    );
}

/// Spend a run's steps up to the point its cadence withdraws the parking tools.
///
/// The rows are written directly rather than driven through the worker: what
/// is under test is the step *after* the cadence is spent, and driving real
/// model steps would only add the time it takes to reach it.
async fn spend_the_cadence(
    store: &Arc<dyn Store>,
    id: tidebreak_core::AgentRunId,
    chat_id: ChatId,
    steps: usize,
) {
    for step in 0..steps {
        let worker_lease = uuid::Uuid::new_v4();
        assert_eq!(
            store
                .claim_agent_run(worker_lease, chrono::Duration::minutes(5), 4, 2)
                .await
                .unwrap()
                .expect("run should be claimable while it still has budget")
                .id,
            id
        );
        let call = tidebreak_core::SandboxToolCallRequest {
            id: CallId::new(),
            agent_run_id: id,
            chat_id,
            provider_id: format!("seeded_search_{step}"),
            name: tidebreak_core::SANDBOX_WEB_SEARCH_TOOL.into(),
            arguments: serde_json::json!({"query": format!("step {step}")}),
        };
        let call_id = call.id;
        store
            .park_agent_run_for_sandbox_tool_calls(
                id,
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
async fn vendor_search_is_withdrawn_with_work_tools_near_the_step_limit() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let chat = sandbox_chat();
    store.create_chat(&chat).await.unwrap();
    let spawn = CallId::new();
    let id = tidebreak_core::AgentRunId::sandbox_for_spawn_call(spawn);
    admit_sandbox(&store, chat.id, spawn, "Research this.").await;
    spend_the_cadence(&store, id, chat.id, 7).await;
    let calls = store
        .list_sandbox_tool_calls_for_agent_run(id)
        .await
        .unwrap();

    let request = sandbox_request(
        &AgentConfig {
            provider: Some(ProviderId::new("anthropic")),
            model: "claude-opus-5".into(),
            max_steps: 8,
            web_search: TurnWebSearch::Vendor(tidebreak_core::VendorWebSearch {
                max_uses: tidebreak_core::VendorWebSearch::DEFAULT_MAX_USES,
            }),
            ..AgentConfig::default()
        },
        "Research this.".into(),
        &calls,
        store.as_ref(),
        false,
        &[],
        &[],
    )
    .await
    .unwrap();

    assert!(request.vendor_web_search.is_none());
    assert!(!request.tools.iter().any(|tool| {
        matches!(
            tool.name.as_str(),
            SANDBOX_EXEC_TOOL
                | tidebreak_core::SANDBOX_WEB_SEARCH_TOOL
                | tidebreak_core::UPDATE_TASK_PLAN_TOOL
        )
    }));
    assert!(request
        .system
        .as_deref()
        .is_some_and(|prompt| !prompt.contains(SANDBOX_PROMPT_WEB_SEARCH_CLAUSE)));
    let notice = request
        .messages
        .last()
        .unwrap()
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<String>();
    assert!(notice.contains("search are no longer available"));
}

/// A run that calls a tool after its cadence is spent checks in, not dies.
///
/// This is the production failure #1829 first patched with a refusal reserve:
/// three background runs spent their whole tool budget producing documents,
/// called a work tool once more after it was withdrawn, and were failed
/// outright — discarding finished work. The cadence design goes further than
/// the refusal did: the run pauses in `needs_input` with a check-in receipt
/// its requester can read, and a resume grants another window (optionally
/// with guidance) under which the run finishes.
#[tokio::test]
async fn a_call_made_after_the_budget_is_spent_is_refused_rather_than_failing_the_run() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let chat = sandbox_chat();
    store.create_chat(&chat).await.unwrap();
    let provider = Arc::new(WebSearchThenFinalProvider::default());
    let worker = SandboxAgentRunWorker::new(
        store.clone(),
        test_secrets(),
        Arc::new(FixedResolver(provider.clone())),
        Arc::new(Notify::new()),
        Arc::new(Notify::new()),
        Arc::new(EventBus::default()),
        AgentConfig {
            model: "sandbox-model".into(),
            max_steps: 8,
            ..AgentConfig::default()
        },
        None,
        SandboxAgentRunWorkerConfig::default(),
    );
    let spawn = CallId::new();
    let id = tidebreak_core::AgentRunId::sandbox_for_spawn_call(spawn);
    admit_sandbox(&store, chat.id, spawn, "Produce a document.").await;
    // Seven of eight steps spent: the next request is the first one whose
    // parking tools are withdrawn (a checkpoint needs the step that parks it
    // and the step that consumes it).
    spend_the_cadence(&store, id, chat.id, 7).await;

    // The model searches anyway. That must pause the run, not kill it.
    assert_eq!(
        worker.run_once().await.unwrap(),
        SandboxAgentRunWorkerOutcome::CheckedIn(id)
    );
    let paused = store.get_agent_run(id).await.unwrap().unwrap();
    assert_eq!(paused.status, AgentRunStatus::NeedsInput);
    assert_eq!(paused.checkin_grants, 0);

    // The request the cadence ran out on withdrew the work tools and said so
    // in words rather than leaving the model to infer a rule from an absence.
    let spent_request = provider.requests.lock().unwrap()[0].clone();
    let offered: Vec<&str> = spent_request
        .tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect();
    assert!(
        !offered.contains(&tidebreak_core::SANDBOX_WEB_SEARCH_TOOL)
            && !offered.contains(&SANDBOX_EXEC_TOOL),
        "work tools should be withdrawn at the cadence, got {offered:?}"
    );
    let notice: String = spent_request
        .messages
        .last()
        .unwrap()
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        notice.contains("entire step budget") && notice.contains("done"),
        "the exhausted request should say the cadence is spent, got {notice:?}"
    );

    // A resume grants another window and folds guidance into the task.
    let resumed = store
        .resume_agent_run_from_checkin(id, Some("Wrap up now: submit what you have."))
        .await
        .unwrap()
        .expect("a paused run resumes");
    assert_eq!(resumed.status, AgentRunStatus::RetryWait);
    assert_eq!(resumed.checkin_grants, 1);
    assert!(resumed
        .input
        .as_deref()
        .unwrap()
        .contains("Wrap up now: submit what you have."));

    // Under the doubled window the run finishes and hands over its work.
    assert_eq!(
        worker.run_once().await.unwrap(),
        SandboxAgentRunWorkerOutcome::Completed(id)
    );
    let resumed_request = provider.requests.lock().unwrap()[1].clone();
    assert!(
        resumed_request
            .messages
            .first()
            .unwrap()
            .content
            .iter()
            .any(
                |block| matches!(block, ContentBlock::Text { text } if text.contains("Wrap up now"))
            ),
        "the resumed request should carry the guidance in its task text"
    );
}

/// The parent model can act on a check-in itself: resume with guidance, or
/// cancel outright — the same transitions the run panel drives, as tools.
#[tokio::test]
async fn parent_tools_resume_and_cancel_a_checked_in_child() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let chat = sandbox_chat();
    store.create_chat(&chat).await.unwrap();
    let provider = Arc::new(WebSearchThenFinalProvider::default());
    let worker = SandboxAgentRunWorker::new(
        store.clone(),
        test_secrets(),
        Arc::new(FixedResolver(provider.clone())),
        Arc::new(Notify::new()),
        Arc::new(Notify::new()),
        Arc::new(EventBus::default()),
        AgentConfig {
            model: "sandbox-model".into(),
            max_steps: 8,
            ..AgentConfig::default()
        },
        None,
        SandboxAgentRunWorkerConfig::default(),
    );
    let spawn = CallId::new();
    let id = tidebreak_core::AgentRunId::sandbox_for_spawn_call(spawn);
    admit_sandbox(&store, chat.id, spawn, "Produce a document.").await;
    spend_the_cadence(&store, id, chat.id, 7).await;
    assert_eq!(
        worker.run_once().await.unwrap(),
        SandboxAgentRunWorkerOutcome::CheckedIn(id)
    );

    let ctx = tidebreak_core::ToolCtx::new_legacy_workspace(chat.id, None, dir.path().join("ws"));
    let resume = crate::agent_control_tools::ResumeAgentTool::new(store.clone());
    let output = tidebreak_core::Tool::execute(
        &resume,
        &ctx,
        serde_json::json!({"agent_id": id, "guidance": "Wrap it up."}),
    )
    .await
    .unwrap();
    assert!(!output.is_error, "resume should succeed: {output:?}");
    let resumed = store.get_agent_run(id).await.unwrap().unwrap();
    assert_eq!(resumed.status, AgentRunStatus::RetryWait);
    assert_eq!(resumed.checkin_grants, 1);

    // Resuming a run that is not paused is a readable refusal, not an error.
    let again = tidebreak_core::Tool::execute(&resume, &ctx, serde_json::json!({"agent_id": id}))
        .await
        .unwrap();
    assert!(again.is_error);

    // Drive the run to its next pause, then cancel it from the parent's side.
    assert_eq!(
        worker.run_once().await.unwrap(),
        SandboxAgentRunWorkerOutcome::Completed(id)
    );
    let cancel = crate::agent_control_tools::CancelAgentTool::new(store.clone());
    let output = tidebreak_core::Tool::execute(
        &cancel,
        &ctx,
        serde_json::json!({"agent_id": id, "reason": "no longer needed"}),
    )
    .await
    .unwrap();
    assert!(
        !output.is_error,
        "cancelling a finished run reads as already finished: {output:?}"
    );
}
