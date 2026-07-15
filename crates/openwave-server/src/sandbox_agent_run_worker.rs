//! Execution for durably claimed sandboxed agent runs.
//!
//! A sandbox run intentionally is not a second foreground turn. It receives
//! only its immutable delegated task, uses one model completion with no tools,
//! and returns bounded text through the fenced agent-run result transition.
//! That keeps the first execution surface small while preserving the same
//! claim, heartbeat, cancellation, and replay mechanics as other workers.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use openwave_core::{
    AgentConfig, AgentError, AgentRun, AgentRunStatus, ChatMessage, ChatRequest,
    FailAgentRunOutcome, ModelProvider, ProviderEvent, Result, Role, StopReason, Store,
    SubmitAgentRunResultOutcome,
};
use tokio::sync::Notify;

use crate::resolver::ProviderResolver;

/// Fixed instruction set for the initial isolated executor.
///
/// Deliberately do not inherit the foreground system prompt: it may describe
/// interactive tools or conversation-wide responsibilities that are outside a
/// depth-one child run's authority.
const SANDBOX_SYSTEM_PROMPT: &str = "You are a sandboxed background agent. Work only on the delegated task below and return a concise, self-contained result for your parent agent. You have no tools, cannot access the conversation, filesystem, network, or other agents, and must not ask to create or call tools.";

#[derive(Debug, Clone, Copy)]
pub(crate) struct SandboxAgentRunWorkerConfig {
    lease: Duration,
    heartbeat: Duration,
    idle_min: Duration,
    idle_cap: Duration,
    failure_delay: Duration,
    max_concurrency: usize,
    max_running_global: u32,
    max_running_per_chat: u32,
    #[cfg(test)]
    suppress_resolver_heartbeats: bool,
}

impl Default for SandboxAgentRunWorkerConfig {
    fn default() -> Self {
        Self {
            lease: Duration::from_secs(60),
            heartbeat: Duration::from_secs(15),
            idle_min: Duration::from_millis(250),
            idle_cap: Duration::from_secs(5),
            failure_delay: Duration::from_secs(1),
            max_concurrency: 4,
            max_running_global: 4,
            max_running_per_chat: 2,
            #[cfg(test)]
            suppress_resolver_heartbeats: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SandboxAgentRunWorkerOutcome {
    Idle,
    Completed(openwave_core::AgentRunId),
    RetryScheduled(openwave_core::AgentRunId),
    Failed(openwave_core::AgentRunId),
    Cancelled(openwave_core::AgentRunId),
    LeaseLost(openwave_core::AgentRunId),
}

#[derive(Clone)]
pub(crate) struct SandboxAgentRunWorker {
    store: Arc<dyn Store>,
    resolver: Arc<dyn ProviderResolver>,
    wake: Arc<Notify>,
    agent_config: AgentConfig,
    /// Each run receives a directory under this private root. The initial
    /// no-tools executor does not open it; retaining the boundary now means a
    /// future sandbox-safe tool adapter must be given an exact per-run handle
    /// rather than a chat or project path.
    private_scratch_root: Option<PathBuf>,
    config: SandboxAgentRunWorkerConfig,
}

impl SandboxAgentRunWorker {
    pub(crate) fn new(
        store: Arc<dyn Store>,
        resolver: Arc<dyn ProviderResolver>,
        wake: Arc<Notify>,
        agent_config: AgentConfig,
        private_scratch_root: Option<PathBuf>,
        config: SandboxAgentRunWorkerConfig,
    ) -> Self {
        assert!(!config.lease.is_zero());
        assert!(!config.heartbeat.is_zero());
        assert!(config.heartbeat < config.lease);
        assert!(config.max_concurrency > 0);
        assert!(config.max_running_global > 0);
        assert!(config.max_running_per_chat > 0);
        Self {
            store,
            resolver,
            wake,
            agent_config,
            private_scratch_root,
            config,
        }
    }

    pub(crate) async fn run(self) {
        let mut lanes = tokio::task::JoinSet::new();
        for _ in 0..self.config.max_concurrency {
            lanes.spawn(self.clone().run_lane());
        }
        while let Some(result) = lanes.join_next().await {
            if let Err(error) = result {
                eprintln!("openwave: sandbox agent worker lane stopped: {error}");
                tokio::time::sleep(self.config.failure_delay).await;
            }
            lanes.spawn(self.clone().run_lane());
        }
    }

    async fn run_lane(self) {
        let mut idle_delay = self.config.idle_min;
        loop {
            match self.run_once().await {
                Ok(SandboxAgentRunWorkerOutcome::Idle) => {
                    tokio::select! {
                        _ = tokio::time::sleep(idle_delay) => {}
                        _ = self.wake.notified() => {}
                    }
                    idle_delay = idle_delay.saturating_mul(2).min(self.config.idle_cap);
                }
                Ok(_) => idle_delay = self.config.idle_min,
                Err(error) => {
                    eprintln!("openwave: sandbox agent worker iteration failed: {error}");
                    tokio::select! {
                        _ = tokio::time::sleep(self.config.failure_delay) => {}
                        _ = self.wake.notified() => {}
                    }
                }
            }
        }
    }

    /// Claim and execute one sandbox run. It is exposed inside the server crate
    /// so focused integration tests can exercise the real durable transitions
    /// without starting permanent worker lanes.
    pub(crate) async fn run_once(&self) -> Result<SandboxAgentRunWorkerOutcome> {
        let lease_token = uuid::Uuid::new_v4();
        let lease = chrono_duration(self.config.lease)?;
        let Some(run) = self
            .store
            .claim_agent_run(
                lease_token,
                lease,
                self.config.max_running_global,
                self.config.max_running_per_chat,
            )
            .await?
        else {
            return Ok(SandboxAgentRunWorkerOutcome::Idle);
        };
        self.wake.notify_one();
        self.process(run, lease_token).await
    }

    async fn process(
        &self,
        run: AgentRun,
        lease_token: uuid::Uuid,
    ) -> Result<SandboxAgentRunWorkerOutcome> {
        if run.status != AgentRunStatus::Running || run.lease_token != Some(lease_token) {
            return Err(AgentError::msg(format!(
                "claimed sandbox agent run {} has an invalid execution identity",
                run.id
            )));
        }
        let task = run.input.clone().ok_or_else(|| {
            AgentError::msg(format!(
                "claimed sandbox agent run {} has no delegated task",
                run.id
            ))
        })?;
        self.prepare_private_scratch(run.id)?;

        let request = sandbox_request(&self.agent_config, task);
        let provider = match self.resolve_provider(run.id, lease_token).await? {
            Ok(provider) => provider,
            Err(outcome) => return Ok(outcome),
        };
        // Resolve may have waited on credentials or provider configuration.
        // Extend and revalidate the exact live lease immediately before the
        // provider can observe a request.
        // Request a strictly later expiry so the storage heartbeat remains
        // monotonic even when SQLite's clock reports the same instant as the
        // claim. It is still the final DB-clock lease proof before egress.
        let pre_egress_lease = self.config.lease.saturating_add(Duration::from_millis(1));
        if !self
            .store
            .heartbeat_agent_run(run.id, lease_token, chrono_duration(pre_egress_lease)?)
            .await?
        {
            return self
                .acknowledge_cancellation_or_lease_loss(run.id, lease_token)
                .await;
        }
        let mut completion = Box::pin(complete_sandbox_task(provider, request));
        let mut heartbeat = tokio::time::interval(self.config.heartbeat);
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        heartbeat.tick().await;

        loop {
            tokio::select! {
                result = &mut completion => match result {
                    Ok(text) => return self.submit_result(run.id, lease_token, text).await,
                    // No terminal failure transition exists yet. Keep the exact
                    // lease alive only until its scheduler expiry; then the
                    // durable claim state machine safely retries or exhausts
                    // the bounded attempt budget. This avoids inventing an
                    // unfenced failure path in the executor.
                    Err(error) => return self.record_failure(run.id, lease_token, error).await,
                },
                _ = heartbeat.tick() => {
                    if self
                        .store
                        .heartbeat_agent_run(run.id, lease_token, chrono_duration(self.config.lease)?)
                        .await?
                    {
                        continue;
                    }
                    return self.acknowledge_cancellation_or_lease_loss(run.id, lease_token).await;
                }
            }
        }
    }

    async fn resolve_provider(
        &self,
        id: openwave_core::AgentRunId,
        lease_token: uuid::Uuid,
    ) -> Result<std::result::Result<Arc<dyn ModelProvider>, SandboxAgentRunWorkerOutcome>> {
        let resolver = self.resolver.resolve();
        tokio::pin!(resolver);
        #[cfg(test)]
        if self.config.suppress_resolver_heartbeats {
            return Ok(Ok(resolver.await));
        }
        let mut heartbeat = tokio::time::interval(self.config.heartbeat);
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        heartbeat.tick().await;
        loop {
            tokio::select! {
                provider = &mut resolver => return Ok(Ok(provider)),
                _ = heartbeat.tick() => {
                    if !self.store.heartbeat_agent_run(id, lease_token, chrono_duration(self.config.lease)?).await? {
                        return Ok(Err(self.acknowledge_cancellation_or_lease_loss(id, lease_token).await?));
                    }
                }
            }
        }
    }

    async fn record_failure(
        &self,
        id: openwave_core::AgentRunId,
        lease_token: uuid::Uuid,
        error: AgentError,
    ) -> Result<SandboxAgentRunWorkerOutcome> {
        let detail = error.to_string();
        let detail = detail
            .chars()
            .take(openwave_core::AgentRun::MAX_ERROR_DETAIL_LEN)
            .collect::<String>();
        match self
            .store
            .fail_agent_run(
                id,
                lease_token,
                "sandbox_execution_failed",
                &detail,
                chrono_duration(self.config.failure_delay)?,
            )
            .await?
        {
            Some(FailAgentRunOutcome::RetryScheduled(_))
            | Some(FailAgentRunOutcome::ExistingRetry(_)) => {
                Ok(SandboxAgentRunWorkerOutcome::RetryScheduled(id))
            }
            Some(FailAgentRunOutcome::Failed(_)) | Some(FailAgentRunOutcome::ExistingFailed(_)) => {
                Ok(SandboxAgentRunWorkerOutcome::Failed(id))
            }
            None => {
                self.acknowledge_cancellation_or_lease_loss(id, lease_token)
                    .await
            }
        }
    }

    async fn submit_result(
        &self,
        id: openwave_core::AgentRunId,
        lease_token: uuid::Uuid,
        text: String,
    ) -> Result<SandboxAgentRunWorkerOutcome> {
        match self
            .store
            .submit_agent_run_result(id, lease_token, &text)
            .await?
        {
            Some(SubmitAgentRunResultOutcome::Completed(_))
            | Some(SubmitAgentRunResultOutcome::Existing(_)) => {
                Ok(SandboxAgentRunWorkerOutcome::Completed(id))
            }
            None => {
                self.acknowledge_cancellation_or_lease_loss(id, lease_token)
                    .await
            }
        }
    }

    async fn acknowledge_cancellation_or_lease_loss(
        &self,
        id: openwave_core::AgentRunId,
        lease_token: uuid::Uuid,
    ) -> Result<SandboxAgentRunWorkerOutcome> {
        let Some(run) = self.store.get_agent_run(id).await? else {
            return Ok(SandboxAgentRunWorkerOutcome::LeaseLost(id));
        };
        if run.status == AgentRunStatus::Cancelling && run.lease_token == Some(lease_token) {
            if self
                .store
                .finish_agent_run_cancellation(id, lease_token)
                .await?
                .is_some()
            {
                return Ok(SandboxAgentRunWorkerOutcome::Cancelled(id));
            }
        }
        Ok(SandboxAgentRunWorkerOutcome::LeaseLost(id))
    }

    fn prepare_private_scratch(&self, id: openwave_core::AgentRunId) -> Result<()> {
        let Some(root) = &self.private_scratch_root else {
            return Ok(());
        };
        let path = root.join("sandbox-runs").join(id.to_string());
        std::fs::create_dir_all(&path).map_err(|error| {
            AgentError::Store(format!(
                "failed to create private sandbox scratch {}: {error}",
                path.display()
            ))
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).map_err(
                |error| {
                    AgentError::Store(format!(
                        "failed to secure private sandbox scratch {}: {error}",
                        path.display()
                    ))
                },
            )?;
        }
        Ok(())
    }
}

fn sandbox_request(config: &AgentConfig, task: String) -> ChatRequest {
    ChatRequest {
        model: config.model.clone(),
        system: Some(SANDBOX_SYSTEM_PROMPT.into()),
        messages: vec![ChatMessage::text(Role::User, task)],
        tools: vec![],
        max_tokens: config.max_tokens,
        temperature: config.temperature,
    }
}

async fn complete_sandbox_task(
    provider: Arc<dyn ModelProvider>,
    request: ChatRequest,
) -> Result<String> {
    let mut stream = provider.stream(request).await?;
    let mut text = String::new();
    while let Some(event) = stream.next().await {
        match event {
            ProviderEvent::TextDelta { text: delta } => {
                text.push_str(&delta);
                if text.chars().count() > AgentRun::MAX_RESULT_LEN {
                    return Err(AgentError::msg(format!(
                        "sandbox agent output exceeds {} characters",
                        AgentRun::MAX_RESULT_LEN
                    )));
                }
            }
            ProviderEvent::ToolCallStarted { .. } | ProviderEvent::ToolCallArgsDelta { .. } => {
                return Err(AgentError::msg(
                    "sandbox agent requested a tool on the no-tools execution surface",
                ));
            }
            ProviderEvent::Stop { reason } => {
                if matches!(reason, StopReason::ToolUse | StopReason::Cancelled) {
                    return Err(AgentError::msg(
                        "sandbox agent did not produce a final result",
                    ));
                }
                if text.trim().is_empty() {
                    return Err(AgentError::msg(
                        "sandbox agent produced an empty final result",
                    ));
                }
                return Ok(text);
            }
            ProviderEvent::ReasoningDelta { .. } | ProviderEvent::Usage(_) => {}
            _ => {
                return Err(AgentError::msg(
                    "sandbox agent provider emitted an unsupported event",
                ))
            }
        }
    }
    Err(AgentError::msg(
        "sandbox agent provider stream ended without a stop event",
    ))
}

fn chrono_duration(duration: Duration) -> Result<chrono::Duration> {
    chrono::Duration::from_std(duration)
        .map_err(|error| AgentError::msg(format!("invalid sandbox-worker duration: {error}")))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use futures::stream::{self, BoxStream};
    use openwave_core::{
        AcceptAgentRunOutcome, AgentRunExecution, CallId, Chat, ChatId, ChatRequest, DbStore,
        ModelProvider, ProviderId, Role, Store,
    };

    use super::*;

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

    struct FailingProvider;

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
            Arc::new(FixedResolver(provider.clone())),
            Arc::new(Notify::new()),
            AgentConfig {
                model: "sandbox-model".into(),
                ..AgentConfig::default()
            },
            Some(dir.path().join("scratch")),
            SandboxAgentRunWorkerConfig::default(),
        );
        (worker, store, provider, chat, dir)
    }

    #[tokio::test]
    async fn completes_a_claimed_run_with_a_no_tools_private_request() {
        let (worker, store, provider, chat, dir) = fixture().await;
        let call = CallId::new();
        let id = openwave_core::AgentRunId::sandbox_for_spawn_call(call);
        let parent = openwave_core::AgentRunId::foreground_for_chat(chat.id);
        assert!(matches!(
            store
                .accept_agent_run(
                    id,
                    chat.id,
                    Some(parent),
                    Some(call),
                    AgentRunExecution::Sandbox,
                    Some("Investigate this in isolation."),
                )
                .await
                .unwrap(),
            AcceptAgentRunOutcome::Accepted(_)
        ));

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
        assert!(requests[0].tools.is_empty());
        assert_eq!(
            requests[0].messages,
            vec![ChatMessage::text(
                Role::User,
                "Investigate this in isolation."
            )]
        );
        assert_eq!(requests[0].system.as_deref(), Some(SANDBOX_SYSTEM_PROMPT));
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
        let id = openwave_core::AgentRunId::sandbox_for_spawn_call(call);
        assert!(matches!(
            store
                .accept_agent_run(
                    id,
                    chat.id,
                    Some(openwave_core::AgentRunId::foreground_for_chat(chat.id)),
                    Some(call),
                    AgentRunExecution::Sandbox,
                    Some("Wait until cancelled."),
                )
                .await
                .unwrap(),
            AcceptAgentRunOutcome::Accepted(_)
        ));
        let started = Arc::new(Notify::new());
        let worker = SandboxAgentRunWorker::new(
            store.clone(),
            Arc::new(FixedResolver(Arc::new(BlockingProvider {
                started: started.clone(),
            }))),
            Arc::new(Notify::new()),
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
            Some(openwave_core::RequestAgentRunCancellationOutcome::Requested(_))
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
                let id = openwave_core::AgentRunId::sandbox_for_spawn_call(call);
                store
                    .accept_agent_run(
                        id,
                        chat.id,
                        Some(openwave_core::AgentRunId::foreground_for_chat(chat.id)),
                        Some(call),
                        AgentRunExecution::Sandbox,
                        Some(&task),
                    )
                    .await
                    .unwrap();
                id
            }
        };
        let first = child("first".into()).await;
        let worker = SandboxAgentRunWorker::new(
            store.clone(),
            Arc::new(FixedResolver(Arc::new(FailingProvider))),
            Arc::new(Notify::new()),
            AgentConfig {
                model: "m".into(),
                ..AgentConfig::default()
            },
            None,
            SandboxAgentRunWorkerConfig {
                failure_delay: Duration::from_secs(1),
                max_concurrency: 1,
                max_running_global: 1,
                max_running_per_chat: 1,
                ..SandboxAgentRunWorkerConfig::default()
            },
        );
        assert_eq!(
            worker.run_once().await.unwrap(),
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
        tokio::time::sleep(Duration::from_millis(1_100)).await;
        assert_eq!(
            worker.run_once().await.unwrap(),
            SandboxAgentRunWorkerOutcome::RetryScheduled(first)
        );
        tokio::time::sleep(Duration::from_millis(1_100)).await;
        assert_eq!(
            worker.run_once().await.unwrap(),
            SandboxAgentRunWorkerOutcome::Failed(first)
        );
        let terminal = store.get_agent_run(first).await.unwrap().unwrap();
        assert_eq!(terminal.status, AgentRunStatus::Failed);
        let inbox = store
            .list_agent_run_inbox(openwave_core::AgentRunId::foreground_for_chat(chat.id))
            .await
            .unwrap();
        assert!(inbox.iter().any(|entry| entry.child_run_id == first
            && entry.result.text.contains("sandbox_execution_failed")));
    }

    #[tokio::test]
    async fn cancellation_while_resolving_prevents_the_provider_request() {
        let (_unused, store, provider, chat, _dir) = fixture().await;
        let call = CallId::new();
        let id = openwave_core::AgentRunId::sandbox_for_spawn_call(call);
        store
            .accept_agent_run(
                id,
                chat.id,
                Some(openwave_core::AgentRunId::foreground_for_chat(chat.id)),
                Some(call),
                AgentRunExecution::Sandbox,
                Some("do not call provider"),
            )
            .await
            .unwrap();
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let worker = SandboxAgentRunWorker::new(
            store.clone(),
            Arc::new(DelayedResolver {
                entered: entered.clone(),
                release: release.clone(),
                provider: provider.clone(),
            }),
            Arc::new(Notify::new()),
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
    async fn resolver_delay_past_the_database_lease_prevents_provider_egress() {
        let (_unused, store, provider, chat, _dir) = fixture().await;
        let call = CallId::new();
        let id = openwave_core::AgentRunId::sandbox_for_spawn_call(call);
        store
            .accept_agent_run(
                id,
                chat.id,
                Some(openwave_core::AgentRunId::foreground_for_chat(chat.id)),
                Some(call),
                AgentRunExecution::Sandbox,
                Some("do not call provider after expiry"),
            )
            .await
            .unwrap();
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let worker = SandboxAgentRunWorker::new(
            store,
            Arc::new(DelayedResolver {
                entered: entered.clone(),
                release: release.clone(),
                provider: provider.clone(),
            }),
            Arc::new(Notify::new()),
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

    #[test]
    fn sandbox_request_does_not_inherit_foreground_system_or_tools() {
        let request = sandbox_request(
            &AgentConfig {
                model: "m".into(),
                system_prompt: Some("foreground only".into()),
                ..AgentConfig::default()
            },
            "task".into(),
        );
        assert_eq!(request.system.as_deref(), Some(SANDBOX_SYSTEM_PROMPT));
        assert!(request.tools.is_empty());
    }

    fn sandbox_chat() -> Chat {
        Chat {
            id: ChatId::new(),
            project_id: None,
            title: Some("sandbox".into()),
            model: Some("model".into()),
            attachment_revision: 0,
            root_attachments: Vec::new(),
            created_at: chrono::Utc::now(),
        }
    }
}
