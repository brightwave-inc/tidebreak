//! Execution for durably claimed sandboxed agent runs.
//!
//! A sandbox run intentionally is not a second foreground turn. It receives
//! only its immutable delegated task, may checkpoint one fixed host-owned web
//! search call, and returns bounded text through the fenced agent-run result
//! transition.
//! That keeps the first execution surface small while preserving the same
//! claim, heartbeat, cancellation, and replay mechanics as other workers.

use std::path::PathBuf;
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use openwave_core::{
    sandbox_folder_access_proposal_tool_spec, sandbox_web_search_tool_spec, AgentConfig,
    AgentError, AgentRun, AgentRunInboxEntry, AgentRunStatus, CallId, ChatMessage, ChatRequest,
    ClaimAgentRunInboxOutcome, ConsumeAgentRunInboxAndResumeTurnOutcome, ContentBlock,
    FailAgentRunOutcome, ModelProvider, ParkSandboxToolCallOutcome, ProviderEvent,
    RequestFolderAccessArgs, Result, ResumeTurnForAgentRunWaitSetOutcome, Role, SandboxToolCall,
    SandboxToolCallRequest, SandboxToolCallStatus, StopReason, Store, SubmitAgentRunResultOutcome,
    ToolCallRecord,
};
use tokio::sync::Notify;

use crate::bus::EventBus;
use crate::resolver::ProviderResolver;
use crate::state::SandboxAttemptGuard;

/// Fixed instruction set for the initial isolated executor.
///
/// Deliberately do not inherit the foreground system prompt: it may describe
/// interactive tools or conversation-wide responsibilities that are outside a
/// depth-one child run's authority.
const SANDBOX_SYSTEM_PROMPT: &str = "You are a sandboxed background agent. Work only on the delegated task below and return a concise, self-contained result for your parent agent. You cannot access the conversation, filesystem, connected folders, or other agents. You may use at most one tool: web_search when current public-web information is necessary, or request_folder_access only to propose that your foreground parent decide whether to ask the user. The proposal grants no access and cannot open a picker. Otherwise finish directly.";
const SANDBOX_WEB_SEARCH_TOOL_LIMIT: usize = 1;

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
    ParentResumed(openwave_core::AgentRunId),
    ParentWaitSetResumed(openwave_core::CallId),
    ToolCheckpointed(openwave_core::CallId),
    LeaseLost(openwave_core::AgentRunId),
}

enum RecoveryCandidate {
    Delivery(Box<AgentRunInboxEntry>),
    WaitSet(openwave_core::AgentRunWaitSetCandidate),
}

#[derive(Clone)]
pub(crate) struct SandboxAgentRunWorker {
    store: Arc<dyn Store>,
    resolver: Arc<dyn ProviderResolver>,
    wake: Arc<Notify>,
    turn_wake: Arc<Notify>,
    events: Arc<EventBus>,
    attempts: Arc<SandboxAttemptGuard>,
    recovery_prefer_wait_sets: Arc<AtomicBool>,
    #[cfg(test)]
    fail_wait_set_resume_responses: Arc<AtomicUsize>,
    agent_config: AgentConfig,
    /// Each run receives a directory under this private root. The initial
    /// no-tools executor does not open it; retaining the boundary now means a
    /// future sandbox-safe tool adapter must be given an exact per-run handle
    /// rather than a chat or project path.
    private_scratch_root: Option<PathBuf>,
    config: SandboxAgentRunWorkerConfig,
}

impl SandboxAgentRunWorker {
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        store: Arc<dyn Store>,
        resolver: Arc<dyn ProviderResolver>,
        wake: Arc<Notify>,
        turn_wake: Arc<Notify>,
        events: Arc<EventBus>,
        agent_config: AgentConfig,
        private_scratch_root: Option<PathBuf>,
        config: SandboxAgentRunWorkerConfig,
    ) -> Self {
        Self::with_attempts(
            store,
            resolver,
            wake,
            turn_wake,
            events,
            Arc::new(SandboxAttemptGuard::default()),
            agent_config,
            private_scratch_root,
            config,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn with_attempts(
        store: Arc<dyn Store>,
        resolver: Arc<dyn ProviderResolver>,
        wake: Arc<Notify>,
        turn_wake: Arc<Notify>,
        events: Arc<EventBus>,
        attempts: Arc<SandboxAttemptGuard>,
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
            turn_wake,
            events,
            attempts,
            recovery_prefer_wait_sets: Arc::new(AtomicBool::new(true)),
            #[cfg(test)]
            fail_wait_set_resume_responses: Arc::new(AtomicUsize::new(0)),
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
        let deliveries = self.store.list_agent_run_inbox_candidates(16).await?;
        let wait_sets = self
            .store
            .list_ready_agent_run_wait_set_candidates(16)
            .await?;
        let mut deliveries = deliveries.into_iter();
        let mut wait_sets = wait_sets.into_iter();
        let mut next_is_wait_set = self
            .recovery_prefer_wait_sets
            .fetch_xor(true, Ordering::Relaxed);
        loop {
            let candidate = if next_is_wait_set {
                wait_sets
                    .next()
                    .map(RecoveryCandidate::WaitSet)
                    .or_else(|| {
                        deliveries
                            .next()
                            .map(Box::new)
                            .map(RecoveryCandidate::Delivery)
                    })
            } else {
                deliveries
                    .next()
                    .map(Box::new)
                    .map(RecoveryCandidate::Delivery)
                    .or_else(|| wait_sets.next().map(RecoveryCandidate::WaitSet))
            };
            let Some(candidate) = candidate else {
                break;
            };
            next_is_wait_set = !next_is_wait_set;
            let outcome = match candidate {
                RecoveryCandidate::Delivery(delivery) => self.resume_parent(*delivery).await?,
                RecoveryCandidate::WaitSet(candidate) => {
                    self.resume_parent_wait_set(candidate.wait_id).await?
                }
            };
            if outcome != SandboxAgentRunWorkerOutcome::Idle {
                return Ok(outcome);
            }
        }
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

    async fn resume_parent_wait_set(
        &self,
        wait_id: CallId,
    ) -> Result<SandboxAgentRunWorkerOutcome> {
        let resume_token = uuid::Uuid::new_v4();
        self.resume_parent_wait_set_with_token(wait_id, resume_token)
            .await
    }

    async fn resume_parent_wait_set_with_token(
        &self,
        wait_id: CallId,
        resume_token: uuid::Uuid,
    ) -> Result<SandboxAgentRunWorkerOutcome> {
        let mut recovering_ambiguous_commit = false;
        let outcome = loop {
            match self
                .resume_parent_wait_set_once(wait_id, resume_token)
                .await
            {
                Ok(outcome) => break outcome,
                Err(error) => {
                    recovering_ambiguous_commit = true;
                    eprintln!(
                        "openwave: wait-set {wait_id} resume failed; retrying exact request: {error}"
                    );
                    tokio::select! {
                        _ = tokio::time::sleep(self.config.failure_delay) => {}
                        _ = self.wake.notified() => {}
                    }
                }
            }
        };
        let Some(outcome) = outcome else {
            return Ok(SandboxAgentRunWorkerOutcome::Idle);
        };
        match outcome {
            ResumeTurnForAgentRunWaitSetOutcome::Resumed { turn, event, .. } => {
                self.publish_parent_wait_set_resume(wait_id, turn.chat_id, event)
            }
            ResumeTurnForAgentRunWaitSetOutcome::Existing { turn, event, .. }
                if recovering_ambiguous_commit =>
            {
                self.publish_parent_wait_set_resume(wait_id, turn.chat_id, event)
            }
            ResumeTurnForAgentRunWaitSetOutcome::Existing { .. }
            | ResumeTurnForAgentRunWaitSetOutcome::NotReady(_)
            | ResumeTurnForAgentRunWaitSetOutcome::TerminalDeliveryMissing { .. } => {
                Ok(SandboxAgentRunWorkerOutcome::Idle)
            }
        }
    }

    fn publish_parent_wait_set_resume(
        &self,
        wait_id: CallId,
        chat_id: openwave_core::ChatId,
        event: openwave_core::SequencedEvent,
    ) -> Result<SandboxAgentRunWorkerOutcome> {
        // The journal remains authoritative for replay. Publish its exact
        // committed event before shortening the ordinary turn worker's next
        // durable scan.
        let _ = self.events.sender(chat_id).send(event);
        self.turn_wake.notify_one();
        Ok(SandboxAgentRunWorkerOutcome::ParentWaitSetResumed(wait_id))
    }

    async fn resume_parent_wait_set_once(
        &self,
        wait_id: CallId,
        resume_token: uuid::Uuid,
    ) -> Result<Option<ResumeTurnForAgentRunWaitSetOutcome>> {
        let outcome = self
            .store
            .resume_turn_for_agent_run_wait_set(wait_id, resume_token)
            .await?;
        #[cfg(test)]
        if matches!(
            outcome,
            Some(
                ResumeTurnForAgentRunWaitSetOutcome::Resumed { .. }
                    | ResumeTurnForAgentRunWaitSetOutcome::Existing { .. }
            )
        ) && self
            .fail_wait_set_resume_responses
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                remaining.checked_sub(1)
            })
            .is_ok()
        {
            return Err(AgentError::Store(
                "injected ambiguous wait-set resume response".into(),
            ));
        }
        Ok(outcome)
    }

    /// Advance one immutable child delivery to a durably resumable parent turn.
    ///
    /// The candidate scan is only recovery/latency plumbing. Exact inbox claim
    /// and consume transitions own the fencing, so concurrent lanes and a
    /// restart after child completion cannot double-resume the foreground turn.
    async fn resume_parent(
        &self,
        delivery: AgentRunInboxEntry,
    ) -> Result<SandboxAgentRunWorkerOutcome> {
        let lease_token = uuid::Uuid::new_v4();
        let lease = chrono_duration(self.config.lease)?;
        let Some(claim) = self
            .store
            .claim_agent_run_inbox_entry(
                delivery.parent_run_id,
                delivery.child_run_id,
                lease_token,
                lease,
            )
            .await?
        else {
            return Ok(SandboxAgentRunWorkerOutcome::Idle);
        };
        let entry = match claim {
            ClaimAgentRunInboxOutcome::Claimed(entry)
            | ClaimAgentRunInboxOutcome::Existing(entry) => entry,
        };
        let Some(outcome) = self
            .store
            .consume_agent_run_inbox_entry_and_resume_turn(
                entry.parent_run_id,
                entry.child_run_id,
                lease_token,
            )
            .await?
        else {
            return Ok(SandboxAgentRunWorkerOutcome::Idle);
        };
        match outcome {
            ConsumeAgentRunInboxAndResumeTurnOutcome::Resumed { .. }
            | ConsumeAgentRunInboxAndResumeTurnOutcome::Existing { .. } => {
                // A committed `resuming` turn is authoritative. This only
                // reduces the latency before the ordinary turn scan sees it.
                self.turn_wake.notify_one();
                Ok(SandboxAgentRunWorkerOutcome::ParentResumed(
                    entry.child_run_id,
                ))
            }
        }
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
        let Some(active_attempt) = self.attempts.register_model(run.id, lease_token) else {
            return Ok(SandboxAgentRunWorkerOutcome::LeaseLost(run.id));
        };
        let cancel = active_attempt.cancel_token();
        // Close cancel-before-register: registration happens before resolver or
        // provider work, then the durable lease is immediately revalidated.
        if !self
            .store
            .heartbeat_agent_run(run.id, lease_token, chrono_duration(self.config.lease)?)
            .await?
        {
            return self
                .acknowledge_cancellation_or_lease_loss(run.id, lease_token)
                .await;
        }
        let task = run.input.clone().ok_or_else(|| {
            AgentError::msg(format!(
                "claimed sandbox agent run {} has no delegated task",
                run.id
            ))
        })?;
        self.prepare_private_scratch(run.id)?;

        let previous_calls = self
            .store
            .list_sandbox_tool_calls_for_agent_run(run.id)
            .await?;
        let request =
            sandbox_request(&self.agent_config, task, &previous_calls, &*self.store).await?;
        let Some(provider) = self.resolve_provider(run.id, lease_token, &cancel).await? else {
            // `resolve_provider` has returned and dropped its resolver future
            // before this durable acknowledgement can terminalize the run.
            return self
                .acknowledge_cancellation_or_lease_loss(run.id, lease_token)
                .await;
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
        let completion_result = {
            let mut completion = Box::pin(complete_sandbox_task(provider, request));
            let mut heartbeat = tokio::time::interval(self.config.heartbeat);
            heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            heartbeat.tick().await;
            loop {
                tokio::select! {
                    result = &mut completion => break Some(result),
                    _ = cancel.cancelled() => break None,
                    _ = heartbeat.tick() => {
                        if self
                            .store
                            .heartbeat_agent_run(run.id, lease_token, chrono_duration(self.config.lease)?)
                            .await?
                        {
                            continue;
                        }
                        break None;
                    }
                }
            }
        };
        // The outbound completion future is out of scope and quiesced before
        // cancellation acknowledgement can commit a terminal durable state.
        let Some(completion_result) = completion_result else {
            return self
                .acknowledge_cancellation_or_lease_loss(run.id, lease_token)
                .await;
        };
        match completion_result {
            Ok(SandboxCompletion::Final(text)) => {
                self.submit_result(run.id, lease_token, text).await
            }
            Ok(SandboxCompletion::WebSearch {
                provider_id,
                arguments,
            }) => {
                self.park_web_search(run, lease_token, provider_id, arguments)
                    .await
            }
            Ok(SandboxCompletion::FolderAccessProposal { request }) => {
                self.submit_folder_access_proposal(run.id, lease_token, request)
                    .await
            }
            Err(error) => self.record_failure(run.id, lease_token, error).await,
        }
    }

    async fn resolve_provider(
        &self,
        id: openwave_core::AgentRunId,
        lease_token: uuid::Uuid,
        cancel: &openwave_core::CancelToken,
    ) -> Result<Option<Arc<dyn ModelProvider>>> {
        let resolver = self.resolver.resolve();
        tokio::pin!(resolver);
        #[cfg(test)]
        if self.config.suppress_resolver_heartbeats {
            return Ok(Some(resolver.await));
        }
        let mut heartbeat = tokio::time::interval(self.config.heartbeat);
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        heartbeat.tick().await;
        loop {
            tokio::select! {
                provider = &mut resolver => return Ok(Some(provider)),
                _ = cancel.cancelled() => return Ok(None),
                _ = heartbeat.tick() => {
                    if !self.store.heartbeat_agent_run(id, lease_token, chrono_duration(self.config.lease)?).await? {
                        return Ok(None);
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

    async fn submit_folder_access_proposal(
        &self,
        id: openwave_core::AgentRunId,
        lease_token: uuid::Uuid,
        request: RequestFolderAccessArgs,
    ) -> Result<SandboxAgentRunWorkerOutcome> {
        match self
            .store
            .submit_agent_run_folder_access_proposal(id, lease_token, &request)
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

    async fn park_web_search(
        &self,
        run: AgentRun,
        lease_token: uuid::Uuid,
        provider_id: String,
        arguments: serde_json::Value,
    ) -> Result<SandboxAgentRunWorkerOutcome> {
        let call = SandboxToolCallRequest {
            id: CallId::new(),
            agent_run_id: run.id,
            chat_id: run.chat_id,
            provider_id,
            name: openwave_core::SANDBOX_WEB_SEARCH_TOOL.into(),
            arguments,
        };
        let outcome = match self
            .store
            .park_agent_run_for_sandbox_tool_call(run.id, lease_token, &call)
            .await
        {
            Ok(outcome) => outcome,
            Err(error) => {
                // The local transaction may have committed before its response
                // was lost. Recover only a checkpoint whose immutable payload
                // and producing lease match this exact model completion; never
                // issue a second model call or tool checkpoint on ambiguity.
                let recovered = self
                    .store
                    .list_sandbox_tool_calls_for_agent_run(run.id)
                    .await?
                    .into_iter()
                    .find(|existing| {
                        existing.park_lease_token == lease_token
                            && existing.provider_id == call.provider_id
                            && existing.name == call.name
                            && existing.arguments == call.arguments
                    });
                if let Some(call) = recovered {
                    self.wake.notify_one();
                    return Ok(SandboxAgentRunWorkerOutcome::ToolCheckpointed(call.id));
                }
                return Err(error);
            }
        };
        match outcome {
            ParkSandboxToolCallOutcome::Parked { call, .. }
            | ParkSandboxToolCallOutcome::Existing { call, .. } => {
                // This shared wake is only a latency hint; the dedicated
                // executor's durable candidate scan remains the recovery path.
                self.wake.notify_one();
                Ok(SandboxAgentRunWorkerOutcome::ToolCheckpointed(call.id))
            }
            ParkSandboxToolCallOutcome::IdentityConflict => {
                self.record_failure(
                    run.id,
                    lease_token,
                    AgentError::msg("sandbox web-search checkpoint identity conflict"),
                )
                .await
            }
            ParkSandboxToolCallOutcome::DelegatedResourceUnavailable => {
                self.record_failure(
                    run.id,
                    lease_token,
                    AgentError::msg("sandbox delegated resource is unavailable"),
                )
                .await
            }
            ParkSandboxToolCallOutcome::LeaseLost => {
                self.acknowledge_cancellation_or_lease_loss(run.id, lease_token)
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
        if run.status == AgentRunStatus::Cancelling
            && run.lease_token == Some(lease_token)
            && self
                .store
                .finish_agent_run_cancellation(id, lease_token)
                .await?
                .is_some()
        {
            return Ok(SandboxAgentRunWorkerOutcome::Cancelled(id));
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

async fn sandbox_request(
    config: &AgentConfig,
    task: String,
    calls: &[SandboxToolCall],
    store: &dyn Store,
) -> Result<ChatRequest> {
    if calls.len() > SANDBOX_WEB_SEARCH_TOOL_LIMIT {
        return Err(AgentError::msg("sandbox web-search tool budget exceeded"));
    }
    if config.max_steps == 0 || calls.len().saturating_add(1) > config.max_steps {
        return Err(AgentError::msg("sandbox model-step budget exceeded"));
    }
    let mut messages = vec![ChatMessage::text(Role::User, task)];
    for call in calls {
        if call.name != openwave_core::SANDBOX_WEB_SEARCH_TOOL || !call.status.is_terminal() {
            return Err(AgentError::msg("sandbox checkpoint cannot be resumed"));
        }
        let receipt = store
            .get_sandbox_tool_call_receipt(call.id)
            .await?
            .ok_or_else(|| AgentError::msg("sandbox checkpoint is missing its receipt"))?;
        messages.push(ChatMessage {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: call.provider_id.clone(),
                name: call.name.clone(),
                input: call.arguments.clone(),
            }],
        });
        messages.push(ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: call.provider_id.clone(),
                content: receipt.result,
                is_error: receipt.status != SandboxToolCallStatus::Completed,
            }],
        });
    }
    Ok(ChatRequest {
        model: config.model.clone(),
        system: Some(SANDBOX_SYSTEM_PROMPT.into()),
        messages,
        // Once a receipt exists, omit the tool definition. This makes the
        // depth-one sandbox's one-call budget visible to the model and turns a
        // second call into a deterministic worker error rather than new work.
        // A tool checkpoint consumes one model completion and a resumed final
        // completion. Never advertise work that the remaining model budget
        // cannot consume after its durable receipt arrives.
        tools: if calls.is_empty() && config.max_steps >= 2 {
            vec![
                sandbox_web_search_tool_spec(),
                sandbox_folder_access_proposal_tool_spec(),
            ]
        } else if calls.is_empty() && config.max_steps >= 1 {
            vec![sandbox_folder_access_proposal_tool_spec()]
        } else {
            vec![]
        },
        max_tokens: config.max_tokens,
        temperature: config.temperature,
    })
}

enum SandboxCompletion {
    Final(String),
    WebSearch {
        provider_id: String,
        arguments: serde_json::Value,
    },
    FolderAccessProposal {
        request: RequestFolderAccessArgs,
    },
}

async fn complete_sandbox_task(
    provider: Arc<dyn ModelProvider>,
    request: ChatRequest,
) -> Result<SandboxCompletion> {
    let web_search_advertised = request
        .tools
        .iter()
        .any(|tool| tool.name == openwave_core::SANDBOX_WEB_SEARCH_TOOL);
    let folder_proposal_advertised = request
        .tools
        .iter()
        .any(|tool| tool.name == openwave_core::REQUEST_FOLDER_ACCESS_TOOL);
    let mut stream = provider.stream(request).await?;
    let mut text = String::new();
    let mut calls = std::collections::BTreeMap::<u32, (String, String, String)>::new();
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
            ProviderEvent::ToolCallStarted { index, id, name } => {
                if calls.insert(index, (id, name, String::new())).is_some() {
                    return Err(AgentError::msg(
                        "sandbox agent emitted duplicate tool-call index",
                    ));
                }
            }
            ProviderEvent::ToolCallArgsDelta { index, fragment } => {
                let Some((_, _, arguments)) = calls.get_mut(&index) else {
                    return Err(AgentError::msg(
                        "sandbox agent emitted tool arguments before its call",
                    ));
                };
                if arguments.len().saturating_add(fragment.len())
                    > ToolCallRecord::MAX_ARGUMENT_BYTES
                {
                    return Err(AgentError::msg(
                        "sandbox agent tool arguments exceed the durable checkpoint limit",
                    ));
                }
                arguments.push_str(&fragment);
            }
            ProviderEvent::Stop { reason } => {
                if matches!(reason, StopReason::Cancelled) {
                    return Err(AgentError::msg(
                        "sandbox agent did not produce a final result",
                    ));
                }
                if reason == StopReason::ToolUse {
                    if !text.is_empty() || calls.len() != 1 {
                        return Err(AgentError::msg(
                            "sandbox agent emitted an ambiguous tool checkpoint",
                        ));
                    }
                    let (_, (provider_id, name, arguments)) = calls.into_iter().next().unwrap();
                    if provider_id.is_empty() {
                        return Err(AgentError::msg(
                            "sandbox agent requested an unavailable tool",
                        ));
                    }
                    if name == openwave_core::SANDBOX_WEB_SEARCH_TOOL && web_search_advertised {
                        let arguments = serde_json::from_str(&arguments).map_err(|_| {
                            AgentError::msg("sandbox agent emitted invalid web-search arguments")
                        })?;
                        return Ok(SandboxCompletion::WebSearch {
                            provider_id,
                            arguments,
                        });
                    }
                    if name == openwave_core::REQUEST_FOLDER_ACCESS_TOOL
                        && folder_proposal_advertised
                    {
                        let request = serde_json::from_str::<RequestFolderAccessArgs>(&arguments)
                            .map_err(|_| {
                            AgentError::msg("sandbox agent emitted invalid folder-access proposal")
                        })?;
                        if !request.is_well_formed() {
                            return Err(AgentError::msg(
                                "sandbox agent emitted invalid folder-access proposal",
                            ));
                        }
                        return Ok(SandboxCompletion::FolderAccessProposal { request });
                    }
                    return Err(AgentError::msg(
                        "sandbox agent requested an unadvertised tool",
                    ));
                }
                if !calls.is_empty() {
                    return Err(AgentError::msg(
                        "sandbox agent stopped with an incomplete tool call",
                    ));
                }
                if text.trim().is_empty() {
                    return Err(AgentError::msg(
                        "sandbox agent produced an empty final result",
                    ));
                }
                return Ok(SandboxCompletion::Final(text));
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
    use std::pin::Pin;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll};

    use async_trait::async_trait;
    use chrono::Utc;
    use futures::stream::{self, BoxStream};
    use openwave_core::{
        AcceptTurnOutcome, AgentRunInboxStatus, CallId, Chat, ChatId, ChatRequest, DbStore,
        ModelProvider, ProviderId, Role, Store, ToolCallResolution, TurnCheckpointProgress, TurnId,
        TurnRunStatus, Usage,
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
                    ProviderEvent::ToolCallStarted {
                        index: 0,
                        id: "search_1".into(),
                        name: openwave_core::SANDBOX_WEB_SEARCH_TOOL.into(),
                    },
                    ProviderEvent::ToolCallArgsDelta {
                        index: 0,
                        fragment: r#"{"query":"OpenWave"}"#.into(),
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
    }

    struct DropAwareStream {
        started: Arc<Notify>,
        dropped: Arc<AtomicBool>,
        announced: bool,
    }

    impl futures::Stream for DropAwareStream {
        type Item = ProviderEvent;

        fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
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
        chat_id: openwave_core::ChatId,
        call: CallId,
        input: &str,
    ) -> openwave_core::AgentRun {
        let running = store
            .list_turn_runs(chat_id)
            .await
            .unwrap()
            .into_iter()
            .find(|turn| turn.status == openwave_core::TurnRunStatus::Running);
        let (turn, lease) = if let Some(turn) = running {
            let lease = turn.lease_token.expect("running test turn has lease");
            (turn, lease)
        } else {
            let turn_id = openwave_core::TurnId::new();
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
                openwave_core::AgentRun::MAX_CONCURRENCY_LIMIT,
                Utc::now(),
            )
            .await
            .unwrap()
            .expect("sandbox test admission should resolve")
        {
            openwave_core::AdmitSandboxAgentRunOutcome::Accepted { child, .. }
            | openwave_core::AdmitSandboxAgentRunOutcome::Existing { child, .. } => child,
            outcome => panic!("unexpected sandbox admission: {outcome:?}"),
        }
    }

    async fn ready_wait_set_for_test(
        store: &Arc<dyn Store>,
        chat_id: openwave_core::ChatId,
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
        store
            .append_turn_event(
                chat_id,
                foreground.id,
                foreground_lease,
                1,
                Utc::now(),
                &openwave_core::AgentEvent::TurnStarted {
                    turn_id: foreground.id,
                },
            )
            .await
            .unwrap();
        store
            .park_turn_for_agent_run_wait_set(
                &openwave_core::AgentRunWaitSetCheckpointRequest {
                    call_id: wait_id,
                    origin_turn_id: foreground.id,
                    child_run_ids: vec![child.id],
                    condition: openwave_core::AgentRunWaitCondition::All,
                    lease_token: foreground_lease,
                    expected_steer_revision: foreground.steer_revision,
                    provider_id: format!("provider-{wait_id}"),
                    arguments: serde_json::json!({"agent_ids": [child.id]}),
                    event_ordinal: 2,
                    progress: TurnCheckpointProgress {
                        model_steps: 1,
                        usage: Usage::default(),
                    },
                },
                Utc::now(),
            )
            .await
            .unwrap();
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
        let id = openwave_core::AgentRunId::sandbox_for_spawn_call(call);
        let parent = openwave_core::AgentRunId::foreground_for_chat(chat.id);
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
            requests[0].tools,
            vec![
                sandbox_web_search_tool_spec(),
                sandbox_folder_access_proposal_tool_spec(),
            ]
        );
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
    async fn checkpoints_one_web_search_and_rebuilds_its_receipt_before_finalizing() {
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
        let id = openwave_core::AgentRunId::sandbox_for_spawn_call(spawn);
        admit_sandbox(&store, chat.id, spawn, "Research this.").await;

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
        assert!(requests[1].tools.is_empty());
        assert!(
            matches!(&requests[1].messages[1].content[0], ContentBlock::ToolUse { id, name, input } if id == "search_1" && name == openwave_core::SANDBOX_WEB_SEARCH_TOOL && input == &serde_json::json!({"query":"OpenWave"}))
        );
        assert!(
            matches!(&requests[1].messages[2].content[0], ContentBlock::ToolResult { tool_use_id, content, is_error } if tool_use_id == "search_1" && content == "{\"results\":[]}" && !is_error)
        );
    }

    #[tokio::test]
    async fn refuses_ambiguous_or_unavailable_sandbox_tool_events_without_checkpointing() {
        let request = ChatRequest {
            model: "m".into(),
            system: None,
            messages: vec![],
            tools: vec![],
            max_tokens: None,
            temperature: None,
        };
        for events in [
            vec![
                ProviderEvent::ToolCallStarted {
                    index: 0,
                    id: "one".into(),
                    name: "web_search".into(),
                },
                ProviderEvent::ToolCallStarted {
                    index: 1,
                    id: "two".into(),
                    name: "web_search".into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::ToolUse,
                },
            ],
            vec![
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
            ],
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
        ] {
            assert!(
                complete_sandbox_task(Arc::new(EventProvider(events)), request.clone())
                    .await
                    .is_err()
            );
        }
    }

    #[tokio::test]
    async fn cancellation_fence_prevents_a_stale_worker_from_parking_web_search() {
        let (worker, store, _provider, chat, _dir) = fixture().await;
        let spawn = CallId::new();
        let id = openwave_core::AgentRunId::sandbox_for_spawn_call(spawn);
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
                .park_web_search(
                    run,
                    lease,
                    "search_1".into(),
                    serde_json::json!({"query":"OpenWave"})
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
    async fn durable_inbox_scan_resumes_a_parked_parent_without_a_wake() {
        let (worker, store, _provider, chat, _dir) = fixture().await;
        let turn_id = TurnId::new();
        assert!(matches!(
            store
                .accept_turn(turn_id, chat.id, "sandbox-model", "delegate")
                .await
                .unwrap(),
            AcceptTurnOutcome::Accepted(_)
        ));
        let foreground_lease = uuid::Uuid::new_v4();
        let now = chrono::Utc::now();
        let foreground = store
            .claim_turn_run(foreground_lease, now, now + chrono::Duration::minutes(1))
            .await
            .unwrap()
            .turn
            .unwrap();
        let call = CallId::new();
        let child_id = openwave_core::AgentRunId::sandbox_for_spawn_call(call);
        assert!(matches!(
            store
                .accept_sandbox_agent_run_and_park_turn(
                    child_id,
                    foreground.id,
                    call,
                    "return the child result",
                    foreground_lease,
                    foreground.steer_revision,
                    TurnCheckpointProgress {
                        model_steps: 1,
                        usage: Usage::default(),
                    },
                    chrono::Utc::now(),
                )
                .await
                .unwrap(),
            Some(openwave_core::AcceptSandboxAgentRunAndParkTurnOutcome::Parked { .. })
        ));
        let child_lease = uuid::Uuid::new_v4();
        assert_eq!(
            store
                .claim_agent_run(child_lease, chrono::Duration::minutes(1), 4, 2)
                .await
                .unwrap()
                .unwrap()
                .id,
            child_id
        );
        store
            .submit_agent_run_result(child_id, child_lease, "child result")
            .await
            .unwrap()
            .expect("exact child lease should complete");

        // No notification is sent: a restarted worker's durable scan must
        // still claim and consume this delivery.
        assert_eq!(
            worker.run_once().await.unwrap(),
            SandboxAgentRunWorkerOutcome::ParentResumed(child_id)
        );
        let parent = openwave_core::AgentRunId::foreground_for_chat(chat.id);
        assert_eq!(
            store.list_agent_run_inbox(parent).await.unwrap()[0].status,
            AgentRunInboxStatus::Consumed
        );
        assert_eq!(
            store.get_turn_run(turn_id).await.unwrap().unwrap().status,
            TurnRunStatus::Resuming
        );
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
            .list_agent_run_inbox(openwave_core::AgentRunId::foreground_for_chat(chat.id))
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

    #[tokio::test]
    async fn mixed_legacy_and_wait_set_recovery_does_not_starve_either_class() {
        let (worker, store, _provider, set_chat, _dir) = fixture().await;
        let set_turn_id = TurnId::new();
        store
            .accept_turn(set_turn_id, set_chat.id, "sandbox-model", "set")
            .await
            .unwrap();
        let set_lease = uuid::Uuid::new_v4();
        let now = Utc::now();
        let set_turn = store
            .claim_turn_run(set_lease, now, now + chrono::Duration::minutes(1))
            .await
            .unwrap()
            .turn
            .unwrap();
        let set_child = admit_sandbox(&store, set_chat.id, CallId::new(), "set child").await;
        let wait_id = CallId::new();
        store
            .append_turn_event(
                set_chat.id,
                set_turn.id,
                set_lease,
                1,
                Utc::now(),
                &openwave_core::AgentEvent::TurnStarted {
                    turn_id: set_turn.id,
                },
            )
            .await
            .unwrap();
        store
            .park_turn_for_agent_run_wait_set(
                &openwave_core::AgentRunWaitSetCheckpointRequest {
                    call_id: wait_id,
                    origin_turn_id: set_turn.id,
                    child_run_ids: vec![set_child.id],
                    condition: openwave_core::AgentRunWaitCondition::All,
                    lease_token: set_lease,
                    expected_steer_revision: set_turn.steer_revision,
                    provider_id: format!("provider-{wait_id}"),
                    arguments: serde_json::json!({"agent_ids": [set_child.id]}),
                    event_ordinal: 2,
                    progress: TurnCheckpointProgress {
                        model_steps: 1,
                        usage: Usage::default(),
                    },
                },
                Utc::now(),
            )
            .await
            .unwrap();
        let child_lease = uuid::Uuid::new_v4();
        assert_eq!(
            store
                .claim_agent_run(child_lease, chrono::Duration::minutes(1), 4, 4)
                .await
                .unwrap()
                .unwrap()
                .id,
            set_child.id
        );
        store
            .submit_agent_run_result(set_child.id, child_lease, "set result")
            .await
            .unwrap();

        let legacy_chat = sandbox_chat();
        store.create_chat(&legacy_chat).await.unwrap();
        let legacy_turn_id = TurnId::new();
        store
            .accept_turn(legacy_turn_id, legacy_chat.id, "sandbox-model", "legacy")
            .await
            .unwrap();
        let legacy_lease = uuid::Uuid::new_v4();
        let now = Utc::now();
        let legacy_turn = store
            .claim_turn_run(legacy_lease, now, now + chrono::Duration::minutes(1))
            .await
            .unwrap()
            .turn
            .unwrap();
        let legacy_call = CallId::new();
        let legacy_child = openwave_core::AgentRunId::sandbox_for_spawn_call(legacy_call);
        store
            .accept_sandbox_agent_run_and_park_turn(
                legacy_child,
                legacy_turn.id,
                legacy_call,
                "legacy child",
                legacy_lease,
                legacy_turn.steer_revision,
                TurnCheckpointProgress {
                    model_steps: 1,
                    usage: Usage::default(),
                },
                Utc::now(),
            )
            .await
            .unwrap();
        let child_lease = uuid::Uuid::new_v4();
        assert_eq!(
            store
                .claim_agent_run(child_lease, chrono::Duration::minutes(1), 4, 4)
                .await
                .unwrap()
                .unwrap()
                .id,
            legacy_child
        );
        store
            .submit_agent_run_result(legacy_child, child_lease, "legacy result")
            .await
            .unwrap();

        assert_eq!(
            worker.run_once().await.unwrap(),
            SandboxAgentRunWorkerOutcome::ParentWaitSetResumed(wait_id)
        );
        assert_eq!(
            worker.run_once().await.unwrap(),
            SandboxAgentRunWorkerOutcome::ParentResumed(legacy_child)
        );
    }

    #[tokio::test]
    async fn folder_proposal_completes_the_child_then_resumes_the_parent_without_a_tool_checkpoint()
    {
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
        let child_id = openwave_core::AgentRunId::sandbox_for_spawn_call(call);
        assert!(matches!(
            store
                .accept_sandbox_agent_run_and_park_turn(
                    child_id,
                    foreground.id,
                    call,
                    "ask whether a folder is needed",
                    foreground_lease,
                    foreground.steer_revision,
                    TurnCheckpointProgress {
                        model_steps: 1,
                        usage: Usage::default()
                    },
                    chrono::Utc::now(),
                )
                .await
                .unwrap(),
            Some(openwave_core::AcceptSandboxAgentRunAndParkTurnOutcome::Parked { .. })
        ));
        let provider = Arc::new(EventProvider(vec![
            ProviderEvent::ToolCallStarted {
                index: 0,
                id: "folder_1".into(),
                name: openwave_core::REQUEST_FOLDER_ACCESS_TOOL.into(),
            },
            ProviderEvent::ToolCallArgsDelta {
                index: 0,
                fragment: r#"{"reason":"Read documents needed for the task","requested_capabilities":["read_files"],"folder_hint":"documents"}"#.into(),
            },
            ProviderEvent::Stop { reason: StopReason::ToolUse },
        ]));
        let worker = SandboxAgentRunWorker::new(
            store.clone(),
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
            .list_agent_run_inbox(openwave_core::AgentRunId::foreground_for_chat(chat.id))
            .await
            .unwrap();
        assert!(matches!(
            &inbox[0].result.payload,
            openwave_core::AgentRunResultPayload::FolderAccessProposal { request }
                if request.reason == "Read documents needed for the task"
        ));
        assert_eq!(
            worker.run_once().await.unwrap(),
            SandboxAgentRunWorkerOutcome::ParentResumed(child_id)
        );
        let messages = store.list_messages(chat.id).await.unwrap();
        let proposal = messages.last().unwrap();
        assert_eq!(proposal.role, Role::System);
        assert!(proposal.content.contains("This grants no access"));
        assert!(proposal
            .content
            .contains("normal request_folder_access tool"));
        assert!(!proposal.content.contains("root_id"));
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
        assert_eq!(
            admit_sandbox(&store, chat.id, call, "Wait until cancelled.")
                .await
                .id,
            id
        );
        let started = Arc::new(Notify::new());
        let worker = SandboxAgentRunWorker::new(
            store.clone(),
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
                admit_sandbox(&store, chat.id, call, &task).await;
                id
            }
        };
        let first = child("first".into()).await;
        let worker = SandboxAgentRunWorker::new(
            store.clone(),
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
        admit_sandbox(&store, chat.id, call, "do not call provider").await;
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
    async fn local_signal_drops_resolver_before_durable_cancellation_ack() {
        let (_unused, store, _provider, chat, _dir) = fixture().await;
        let call = CallId::new();
        let id = openwave_core::AgentRunId::sandbox_for_spawn_call(call);
        admit_sandbox(&store, chat.id, call, "cancel resolver").await;
        let entered = Arc::new(Notify::new());
        let dropped = Arc::new(AtomicBool::new(false));
        let attempts = Arc::new(SandboxAttemptGuard::default());
        let worker = SandboxAgentRunWorker::with_attempts(
            store.clone(),
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
            SandboxAgentRunWorkerConfig::default(),
        );
        let entered_wait = entered.notified();
        let execution = tokio::spawn(async move { worker.run_once().await });
        entered_wait.await;
        assert!(matches!(
            store.request_agent_run_cancellation(id).await.unwrap(),
            Some(openwave_core::RequestAgentRunCancellationOutcome::Requested(_))
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
        let id = openwave_core::AgentRunId::sandbox_for_spawn_call(call);
        admit_sandbox(&store, chat.id, call, "cancel completion").await;
        let started = Arc::new(Notify::new());
        let dropped = Arc::new(AtomicBool::new(false));
        let attempts = Arc::new(SandboxAttemptGuard::default());
        let worker = SandboxAgentRunWorker::with_attempts(
            store.clone(),
            Arc::new(FixedResolver(Arc::new(DropAwareProvider {
                started: started.clone(),
                dropped: dropped.clone(),
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
            SandboxAgentRunWorkerConfig::default(),
        );
        let started_wait = started.notified();
        let execution = tokio::spawn(async move { worker.run_once().await });
        started_wait.await;
        assert!(matches!(
            store.request_agent_run_cancellation(id).await.unwrap(),
            Some(openwave_core::RequestAgentRunCancellationOutcome::Requested(_))
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
    async fn cancellation_before_local_registration_is_closed_by_first_heartbeat() {
        let (_unused, store, provider, chat, _dir) = fixture().await;
        let call = CallId::new();
        let id = openwave_core::AgentRunId::sandbox_for_spawn_call(call);
        admit_sandbox(&store, chat.id, call, "cancel before register").await;
        let claimed = store
            .claim_agent_run(uuid::Uuid::new_v4(), chrono::Duration::minutes(5), 1, 1)
            .await
            .unwrap()
            .unwrap();
        let lease = claimed.lease_token.unwrap();
        assert!(matches!(
            store.request_agent_run_cancellation(id).await.unwrap(),
            Some(openwave_core::RequestAgentRunCancellationOutcome::Requested(_))
        ));
        let worker = SandboxAgentRunWorker::new(
            store.clone(),
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
        )
        .await
        .unwrap();
        assert_eq!(request.system.as_deref(), Some(SANDBOX_SYSTEM_PROMPT));
        assert_eq!(
            request.tools,
            vec![
                sandbox_web_search_tool_spec(),
                sandbox_folder_access_proposal_tool_spec(),
            ]
        );
        assert!(request
            .tools
            .iter()
            .all(|tool| tool.name != openwave_core::SANDBOX_READ_DELEGATED_FILE_TOOL));
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
                ..AgentConfig::default()
            },
            "task".into(),
            &[],
            &store,
        )
        .await
        .unwrap();
        assert_eq!(
            request.tools,
            vec![sandbox_folder_access_proposal_tool_spec()]
        );
        let provider = Arc::new(EventProvider(vec![
            ProviderEvent::ToolCallStarted {
                index: 0,
                id: "search_1".into(),
                name: openwave_core::SANDBOX_WEB_SEARCH_TOOL.into(),
            },
            ProviderEvent::ToolCallArgsDelta {
                index: 0,
                fragment: r#"{"query":"OpenWave"}"#.into(),
            },
            ProviderEvent::Stop {
                reason: StopReason::ToolUse,
            },
        ]));
        assert!(complete_sandbox_task(provider, request).await.is_err());
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
