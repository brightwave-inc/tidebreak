//! Claimed sandbox agent-run worker loop.

use std::future::Future;
use std::path::PathBuf;
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tidebreak_core::storage::RecordAgentRunModelStepOutcome;
use tidebreak_core::{
    AgentConfig, AgentError, AgentRun, AgentRunExecutionLocation, AgentRunStatus,
    AgentRunSubmittedOutput, CallId, FailAgentRunOutcome, ModelProvider,
    ParkSandboxToolCallOutcome, RequestFolderAccessArgs, Result,
    ResumeTurnForAgentRunWaitSetOutcome, SandboxToolCallParkEntry, SandboxToolCallRequest,
    SecretProvider, Store, SubmitAgentRunResultOutcome, ToolCallResolution, Usage,
};
use tokio::sync::Notify;

use crate::bus::EventBus;
use crate::resolver::ProviderResolver;
use crate::retry::{LaneBackoff, RetryAttempt};
use crate::state::SandboxAttemptGuard;

use super::config::*;
use super::model_step::*;

/// The receipt code the plan reminder writes, and the exact marker that says a
/// run has already had its one push-back.
const TASK_PLAN_INCOMPLETE: &str = "task_plan_incomplete";

/// Final cancellation accounting and acknowledgement are exact, idempotent
/// CAS operations. Retry transient storage failures responsively, but keep the
/// absolute run deadline as the hard bound on finalization authority.
const CANCELLATION_FINALIZATION_RETRY_INITIAL: Duration = Duration::from_millis(10);
const CANCELLATION_FINALIZATION_RETRY_MAX: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CancellationFinalizationState {
    OwnedCancelling,
    Cancelled,
    Lost,
}

/// Bound the error excerpt a check-in carries: enough to act on, never a blob.
fn truncate_checkin_detail(text: &str) -> String {
    const MAX: usize = 1_000;
    if text.chars().count() <= MAX {
        return text.to_owned();
    }
    let cut: String = text.chars().take(MAX).collect();
    format!("{cut}…")
}

impl SandboxAgentRunWorker {
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        store: Arc<dyn Store>,
        secrets: Arc<dyn SecretProvider>,
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
            secrets,
            resolver,
            wake,
            turn_wake,
            events,
            Arc::new(SandboxAttemptGuard::default()),
            agent_config,
            private_scratch_root,
            None,
            config,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn with_attempts(
        store: Arc<dyn Store>,
        secrets: Arc<dyn SecretProvider>,
        resolver: Arc<dyn ProviderResolver>,
        wake: Arc<Notify>,
        turn_wake: Arc<Notify>,
        events: Arc<EventBus>,
        attempts: Arc<SandboxAttemptGuard>,
        agent_config: AgentConfig,
        private_scratch_root: Option<PathBuf>,
        code_execution: Option<Arc<crate::code_execution::ConfiguredExecProvider>>,
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
            secrets,
            resolver,
            wake,
            turn_wake,
            events,
            attempts,
            #[cfg(test)]
            fail_wait_set_resume_responses: Arc::new(AtomicUsize::new(0)),
            #[cfg(test)]
            fail_cancellation_accounting: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            #[cfg(test)]
            cancellation_accounting_failure_observed: Arc::new(Notify::new()),
            #[cfg(test)]
            cancellation_accounting_calls: Arc::new(AtomicUsize::new(0)),
            agent_config,
            private_scratch_root,
            code_execution,
            config,
        }
    }

    pub(crate) async fn run(self) {
        let mut lanes = tokio::task::JoinSet::new();
        for _ in 0..self.config.max_concurrency {
            lanes.spawn(self.clone().run_lane());
        }
        let mut restart_backoff =
            LaneBackoff::new(self.config.failure_delay, self.config.failure_delay_cap);
        while let Some(result) = lanes.join_next().await {
            if let Err(error) = result {
                tracing::error!("tidebreak: sandbox agent worker lane stopped: {error}");
                tokio::time::sleep(restart_backoff.next_delay()).await;
            }
            lanes.spawn(self.clone().run_lane());
        }
    }

    async fn run_lane(self) {
        let mut idle_delay = self.config.idle_min;
        let mut failure_backoff =
            LaneBackoff::new(self.config.failure_delay, self.config.failure_delay_cap);
        loop {
            match self.run_once().await {
                Ok(SandboxAgentRunWorkerOutcome::Idle) => {
                    failure_backoff.reset();
                    tokio::select! {
                        _ = tokio::time::sleep(idle_delay) => {}
                        _ = self.wake.notified() => {}
                    }
                    idle_delay = idle_delay.saturating_mul(2).min(self.config.idle_cap);
                }
                Ok(_) => {
                    failure_backoff.reset();
                    idle_delay = self.config.idle_min;
                }
                Err(error) => {
                    tracing::warn!("tidebreak: sandbox agent worker iteration failed: {error}");
                    let delay = failure_backoff.next_delay();
                    tokio::select! {
                        _ = tokio::time::sleep(delay) => {}
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
        let wait_sets = self
            .store
            .list_ready_agent_run_wait_set_candidates(16)
            .await?;
        for candidate in wait_sets {
            let outcome = self.resume_parent_wait_set(candidate.wait_id).await?;
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

    pub(super) async fn resume_parent_wait_set_with_token(
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
                    tracing::warn!(
                        "tidebreak: wait-set {wait_id} resume failed; retrying exact request: {error}"
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
        chat_id: tidebreak_core::ChatId,
        event: tidebreak_core::SequencedEvent,
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

    pub(super) async fn process(
        &self,
        run: AgentRun,
        lease_token: uuid::Uuid,
    ) -> Result<SandboxAgentRunWorkerOutcome> {
        self.process_with_pre_egress_hook(run, lease_token, std::future::ready(()))
            .await
    }

    /// Test seam for signalling cancellation in the exact interval after the
    /// final durable lease proof and before provider egress is first polled.
    #[cfg(test)]
    pub(super) async fn process_after_pre_egress<F>(
        &self,
        run: AgentRun,
        lease_token: uuid::Uuid,
        pre_egress_hook: F,
    ) -> Result<SandboxAgentRunWorkerOutcome>
    where
        F: Future<Output = ()>,
    {
        self.process_with_pre_egress_hook(run, lease_token, pre_egress_hook)
            .await
    }

    async fn process_with_pre_egress_hook<F>(
        &self,
        run: AgentRun,
        lease_token: uuid::Uuid,
        pre_egress_hook: F,
    ) -> Result<SandboxAgentRunWorkerOutcome>
    where
        F: Future<Output = ()>,
    {
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
            .renew_or_validate_execution(run.id, lease_token, self.config.lease)
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
        let chat = self.store.get_chat(run.chat_id).await?.ok_or_else(|| {
            AgentError::msg(format!("claimed sandbox agent run {} has no chat", run.id))
        })?;
        let delegated_file_available = if self.config.delegated_file_executor_enabled {
            match self.store.get_sandbox_agent_admission(run.id).await? {
                Some(admission) => delegated_file_admission_matches(&run, &admission, &chat),
                None => false,
            }
        } else {
            false
        };
        // A sandbox child runs the conversation's model, not the boot default.
        // Admission froze the origin turn's selection on the run; only a run
        // admitted before that was recorded resolves the chat's model here.
        let model = match run.model.clone() {
            Some(model) => model,
            None => {
                crate::routes::resolve_chat_model(&*self.store, &chat, &self.agent_config.model)
                    .await?
            }
        };
        let mut agent_config = self.agent_config.clone();
        // The check-in cadence is a live setting read on every claim, so
        // raising it rescues an in-flight run at its next claim instead of
        // waiting for a restart. Absent a stored choice the boot
        // configuration's step budget stands.
        if let Some(steps) =
            crate::routes::read_sandbox_agent_checkin_steps_override(&*self.store).await?
        {
            agent_config.max_steps = steps as usize;
        }
        // Each check-in resume grants one more full window, recorded durably
        // on the run so a replayed claim computes the same budget.
        agent_config.max_steps = agent_config.max_steps.saturating_mul(
            usize::try_from(run.checkin_grants)
                .unwrap_or(0)
                .saturating_add(1),
        );
        // A run whose trailing tool calls all failed needs direction, not more
        // of the same: check in with the requester before spending another
        // model step. Only rows past the last resume count, so the errors one
        // check-in reported cannot re-trigger the next.
        let error_threshold =
            crate::routes::read_sandbox_agent_error_checkin(&*self.store).await? as usize;
        {
            let watermark = usize::try_from(run.checkin_watermark).unwrap_or(0);
            let trailing = previous_calls
                .iter()
                .rev()
                .take_while(|call| call.status == tidebreak_core::SandboxToolCallStatus::Failed)
                .count();
            let past_watermark = previous_calls.len().saturating_sub(watermark);
            if trailing.min(past_watermark) >= error_threshold {
                let last_error = match previous_calls.last() {
                    Some(call) => self
                        .store
                        .get_sandbox_tool_call_receipt(call.id)
                        .await?
                        .map(|receipt| receipt.result)
                        .unwrap_or_default(),
                    None => String::new(),
                };
                let steps_used = sandbox_call_steps(&previous_calls).len();
                return self
                    .submit_checkin(
                        &run,
                        lease_token,
                        tidebreak_core::AgentRunCheckInReason::ConsecutiveToolErrors,
                        steps_used,
                        &format!(
                            "The last {trailing} tool call(s) all failed. Most recent error: \
                             {last}",
                            last = truncate_checkin_detail(&last_error),
                        ),
                    )
                    .await;
            }
        }
        let search_capabilities = if self.resolver.enforces_model_registry() {
            // Sandbox runs resolve without a caller snapshot: on a hosted
            // machine their model path has no per-caller route yet either.
            // Decision 62 names this gap and leaves it open.
            let Some(policy) =
                crate::providers::resolve_model_policy(&*self.store, &model, true, None).await?
            else {
                return Err(AgentError::config(
                    "sandbox model is not registered for its provider",
                ));
            };
            if !crate::providers::is_valid_execution_policy(&policy) {
                return Err(AgentError::config(
                    "managed gateway execution requires a frozen model identity",
                ));
            }
            let capabilities = (
                policy.supports_vendor_web_search,
                policy.supports_search_subrequest,
            );
            crate::providers::apply_model_policy(
                &mut agent_config,
                &policy,
                chat.reasoning_effort,
            )?;
            capabilities
        } else {
            crate::providers::apply_free_form_model(
                &mut agent_config,
                model,
                chat.reasoning_effort,
            )?;
            // A model reached without the registry claims nothing, here as
            // everywhere else: no row asserts that its adapter emits a
            // provider-executed search, or that its provider would accept a
            // search sub-request, so this run is offered neither.
            (false, false)
        };
        // One host setting governs both surfaces. A background run is the
        // conversation's own work delegated to a child, so the operator's
        // single web-search choice decides which search it gets, resolved
        // against the model that is about to run rather than the boot default.
        agent_config.web_search = crate::web_search::resolve_turn_web_search(
            &*self.store,
            &*self.secrets,
            search_capabilities.0,
            search_capabilities.1,
        )
        .await?;
        // Same source the foreground turn freezes into its operating prompt:
        // the host code-execution provider's enabled skill/plugin catalogs.
        // Absent a provider (headless without exec), the run gets an empty
        // catalog and the skills section is omitted — never a fake list.
        let (skills, plugins) = match self.code_execution.as_ref() {
            Some(provider) => (
                provider.skill_catalog().await,
                provider.plugin_catalog().await,
            ),
            None => (Vec::new(), Vec::new()),
        };
        let request = sandbox_request(
            &agent_config,
            task,
            &previous_calls,
            &*self.store,
            delegated_file_available,
            &skills,
            &plugins,
        )
        .await?;
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
            .renew_or_validate_execution(run.id, lease_token, pre_egress_lease)
            .await?
        {
            return self
                .acknowledge_cancellation_or_lease_loss(run.id, lease_token)
                .await;
        }
        pre_egress_hook.await;
        if cancel.is_cancelled() {
            return self
                .finalize_cancellation_after_quiescence(
                    &run,
                    lease_token,
                    SandboxStepAccountingSnapshot::default(),
                )
                .await;
        }
        let step_accounting = SandboxStepAccounting::default();
        let completion_result = {
            let mut completion = Box::pin(complete_sandbox_task_with_accounting(
                provider,
                request,
                step_accounting.clone(),
            ));
            let mut heartbeat = tokio::time::interval(self.config.heartbeat);
            heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            heartbeat.tick().await;
            loop {
                if cancel.is_cancelled() {
                    break None;
                }
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => break None,
                    result = &mut completion => break Some(result),
                    _ = heartbeat.tick() => {
                        if self
                            .renew_or_validate_execution(run.id, lease_token, self.config.lease)
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
            let accounting = step_accounting.snapshot();
            return self
                .finalize_cancellation_after_quiescence(&run, lease_token, accounting)
                .await;
        };
        let SandboxStepAttempt {
            outcome: step,
            usage,
            account,
        } = completion_result;
        let run = if account {
            let Some(run) = self.account_model_step(&run, lease_token, usage).await? else {
                return self
                    .acknowledge_cancellation_or_lease_loss(run.id, lease_token)
                    .await;
            };
            run
        } else {
            run
        };
        let step = match step {
            Ok(step) => step,
            Err(error) => return self.record_failure(&run, lease_token, error).await,
        };
        let SandboxStep {
            narration,
            completion,
            provider_executed,
        } = step;
        let run_id = run.id;
        // Progress keys are per model step, not per parked row: one step can now
        // park several calls, and a replayed claim must land on the same key.
        let depth = sandbox_call_steps(&previous_calls).len();
        let outcome = match completion {
            SandboxCompletion::Final(text) => self.submit_result(&run, lease_token, text).await,
            SandboxCompletion::ToolCalls(intents) => {
                self.park_sandbox_tool_calls(
                    run,
                    lease_token,
                    intents,
                    depth,
                    agent_config.max_steps,
                    &narration,
                )
                .await
            }
            SandboxCompletion::Done {
                provider_id,
                arguments,
                outputs,
                summary,
            } => match self
                .done_plan_reminder(&run, &previous_calls, depth, agent_config.max_steps)
                .await?
            {
                Some(message) => {
                    let intent = SandboxToolCallIntent {
                        provider_id,
                        name: tidebreak_core::SANDBOX_DONE_TOOL.to_owned(),
                        arguments,
                        disposition: SandboxToolCallDisposition::Rejected {
                            error_code: TASK_PLAN_INCOMPLETE,
                            message,
                        },
                    };
                    self.park_sandbox_tool_calls(
                        run,
                        lease_token,
                        vec![intent],
                        depth,
                        agent_config.max_steps,
                        "",
                    )
                    .await
                }
                None => {
                    self.submit_submission(&run, lease_token, &outputs, &summary)
                        .await
                }
            },
            SandboxCompletion::FolderAccessProposal { request } => {
                self.submit_folder_access_proposal(run.id, lease_token, request)
                    .await
            }
        };
        // Published after the step's own durable transition has committed, for
        // the same reason narration is: this is observation, and a failure here
        // must not disturb a transition that already succeeded.
        self.publish_provider_executed(run_id, depth, &provider_executed)
            .await;
        outcome
    }

    /// Commit one provider step's disjoint usage before any checkpoint,
    /// retry, check-in, or terminal transition can advance the run.
    ///
    /// The store operation is keyed by the cumulative state in `run`. If its
    /// transaction committed but the response was lost, the read-back below
    /// recognizes only the exact next totals and continues with this same
    /// completion rather than issuing another provider request.
    async fn account_model_step(
        &self,
        run: &AgentRun,
        lease_token: uuid::Uuid,
        usage: Usage,
    ) -> Result<Option<AgentRun>> {
        #[cfg(test)]
        {
            self.cancellation_accounting_calls
                .fetch_add(1, Ordering::SeqCst);
            if self.fail_cancellation_accounting.load(Ordering::SeqCst) {
                self.cancellation_accounting_failure_observed.notify_one();
                return Err(AgentError::Store(
                    "injected transient in-process cancellation accounting failure".into(),
                ));
            }
        }
        let expected_steps = run.model_steps;
        let expected_usage = run.usage;
        let next_usage = expected_usage
            .checked_add(usage)
            .ok_or_else(|| AgentError::msg("sandbox provider usage total overflowed"))?;
        let next_steps = expected_steps
            .checked_add(1)
            .ok_or_else(|| AgentError::msg("sandbox model-step total overflowed"))?;
        match self
            .store
            .record_agent_run_model_step(
                run.id,
                lease_token,
                expected_steps,
                expected_usage,
                usage,
            )
            .await
        {
            Ok(RecordAgentRunModelStepOutcome::Recorded(run))
            | Ok(RecordAgentRunModelStepOutcome::Existing(run)) => Ok(Some(run)),
            Ok(RecordAgentRunModelStepOutcome::LeaseLost) => Ok(None),
            Ok(RecordAgentRunModelStepOutcome::IdentityConflict(current)) => {
                Err(AgentError::msg(format!(
                    "sandbox model-step accounting identity conflicted: expected step {next_steps}, found {}",
                    current.model_steps
                )))
            }
            Err(error) => {
                let recovered = self.store.get_agent_run(run.id).await?.filter(|current| {
                    current.model_steps == next_steps && current.usage == next_usage
                });
                match recovered {
                    Some(run) => Ok(Some(run)),
                    None => Err(error),
                }
            }
        }
    }

    /// After the provider future has dropped, preserve every observed step and
    /// then commit the immutable cancellation receipt. Cancellation freezes the
    /// exact lease token and claim provenance, so the dedicated renewal CAS may
    /// reopen finalization authority after the execution lease expires without
    /// authorizing another provider request.
    async fn finalize_cancellation_after_quiescence(
        &self,
        run: &AgentRun,
        lease_token: uuid::Uuid,
        accounting: SandboxStepAccountingSnapshot,
    ) -> Result<SandboxAgentRunWorkerOutcome> {
        if accounting.account
            && !self
                .account_cancelled_model_step_with_retry(run, lease_token, accounting.usage)
                .await?
        {
            return Ok(SandboxAgentRunWorkerOutcome::LeaseLost(run.id));
        }
        self.finish_cancellation_with_retry(run.id, lease_token, run.deadline_at)
            .await
    }

    /// Retry one exact baseline-plus-delta accounting identity. Reusing the
    /// original `run` snapshot is intentional: a lost response recovers as
    /// `Existing`, while a second model step cannot match this identity.
    async fn account_cancelled_model_step_with_retry(
        &self,
        run: &AgentRun,
        lease_token: uuid::Uuid,
        usage: Usage,
    ) -> Result<bool> {
        let mut delay = CANCELLATION_FINALIZATION_RETRY_INITIAL;
        loop {
            let mut retry_error = match self.account_model_step(run, lease_token, usage).await {
                Ok(Some(_)) => return Ok(true),
                Ok(None) => None,
                Err(error) => Some(error),
            };

            match self
                .cancellation_finalization_state(run.id, lease_token)
                .await
            {
                Ok(CancellationFinalizationState::OwnedCancelling) => {
                    match self
                        .renew_cancellation_finalization(run.id, lease_token)
                        .await
                    {
                        Ok(true) => {}
                        Ok(false) => return Ok(false),
                        Err(error) => retry_error = Some(error),
                    }
                }
                Ok(
                    CancellationFinalizationState::Cancelled | CancellationFinalizationState::Lost,
                ) => return Ok(false),
                Err(error) => retry_error = Some(error),
            }

            if !Self::wait_for_cancellation_finalization_retry(run.deadline_at, &mut delay).await {
                return Err(retry_error.unwrap_or_else(|| {
                    AgentError::Store(format!(
                        "sandbox cancellation accounting reached the deadline for {}",
                        run.id
                    ))
                }));
            }
        }
    }

    /// Commit or recover the exact cancellation receipt. A store error can be
    /// an ambiguous successful commit, so an exact `cancelled` row is retried
    /// until `finish_agent_run_cancellation` returns its `Existing` receipt.
    async fn finish_cancellation_with_retry(
        &self,
        id: tidebreak_core::AgentRunId,
        lease_token: uuid::Uuid,
        deadline_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<SandboxAgentRunWorkerOutcome> {
        let mut delay = CANCELLATION_FINALIZATION_RETRY_INITIAL;
        loop {
            match self
                .store
                .finish_agent_run_cancellation(id, lease_token)
                .await
            {
                Ok(Some(_)) => return Ok(SandboxAgentRunWorkerOutcome::Cancelled(id)),
                Ok(None) => {
                    let retry_error =
                        match self.cancellation_finalization_state(id, lease_token).await {
                            Ok(CancellationFinalizationState::OwnedCancelling) => {
                                match self.renew_cancellation_finalization(id, lease_token).await {
                                    Ok(true) => None,
                                    Ok(false) => {
                                        return Ok(SandboxAgentRunWorkerOutcome::LeaseLost(id));
                                    }
                                    Err(error) => Some(error),
                                }
                            }
                            Ok(
                                CancellationFinalizationState::Cancelled
                                | CancellationFinalizationState::Lost,
                            ) => return Ok(SandboxAgentRunWorkerOutcome::LeaseLost(id)),
                            Err(error) => Some(error),
                        };
                    if !Self::wait_for_cancellation_finalization_retry(deadline_at, &mut delay)
                        .await
                    {
                        return match retry_error {
                            Some(error) => Err(error),
                            None => Ok(SandboxAgentRunWorkerOutcome::LeaseLost(id)),
                        };
                    }
                }
                Err(mut error) => {
                    match self.cancellation_finalization_state(id, lease_token).await {
                        Ok(CancellationFinalizationState::OwnedCancelling) => {
                            match self.renew_cancellation_finalization(id, lease_token).await {
                                Ok(true) => {}
                                Ok(false) => {
                                    return Ok(SandboxAgentRunWorkerOutcome::LeaseLost(id));
                                }
                                Err(renewal_error) => error = renewal_error,
                            }
                        }
                        // The first finish may have committed before its
                        // response was lost. Retry the exact identity so the
                        // durable method can return `Existing`.
                        Ok(CancellationFinalizationState::Cancelled) => {}
                        Ok(CancellationFinalizationState::Lost) => {
                            return Ok(SandboxAgentRunWorkerOutcome::LeaseLost(id));
                        }
                        Err(state_error) => error = state_error,
                    }
                    if !Self::wait_for_cancellation_finalization_retry(deadline_at, &mut delay)
                        .await
                    {
                        return Err(error);
                    }
                }
            }
        }
    }

    async fn cancellation_finalization_state(
        &self,
        id: tidebreak_core::AgentRunId,
        lease_token: uuid::Uuid,
    ) -> Result<CancellationFinalizationState> {
        let Some(run) = self.store.get_agent_run(id).await? else {
            return Ok(CancellationFinalizationState::Lost);
        };
        let exact_token = run.lease_token == Some(lease_token);
        let deadline_open = run
            .deadline_at
            .is_some_and(|deadline| deadline > chrono::Utc::now());
        Ok(match run.status {
            AgentRunStatus::Cancelling if exact_token && deadline_open => {
                CancellationFinalizationState::OwnedCancelling
            }
            AgentRunStatus::Cancelled => CancellationFinalizationState::Cancelled,
            _ => CancellationFinalizationState::Lost,
        })
    }

    async fn renew_cancellation_finalization(
        &self,
        id: tidebreak_core::AgentRunId,
        lease_token: uuid::Uuid,
    ) -> Result<bool> {
        // Keep finalization authority wider than the retry ceiling so an
        // unusually short execution lease cannot expire between renewal and
        // the next accounting/terminal CAS.
        let finalization_lease = self
            .config
            .lease
            .max(CANCELLATION_FINALIZATION_RETRY_MAX.saturating_mul(2));
        self.store
            .renew_agent_run_cancellation_finalization(
                id,
                lease_token,
                chrono_duration(finalization_lease)?,
            )
            .await
    }

    async fn wait_for_cancellation_finalization_retry(
        deadline_at: Option<chrono::DateTime<chrono::Utc>>,
        delay: &mut Duration,
    ) -> bool {
        let Some(deadline_at) = deadline_at else {
            return false;
        };
        let Ok(remaining) = deadline_at
            .signed_duration_since(chrono::Utc::now())
            .to_std()
        else {
            return false;
        };
        if remaining.is_zero() {
            return false;
        }
        let sleep_for = (*delay).min(remaining);
        tokio::time::sleep(sleep_for).await;
        if sleep_for == remaining {
            return false;
        }
        *delay = delay
            .saturating_mul(2)
            .min(CANCELLATION_FINALIZATION_RETRY_MAX);
        true
    }

    #[cfg(test)]
    pub(super) fn fail_cancellation_accounting_until_released(&self) {
        self.fail_cancellation_accounting
            .store(true, Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(super) fn release_cancellation_accounting(&self) {
        self.fail_cancellation_accounting
            .store(false, Ordering::SeqCst);
    }

    async fn resolve_provider(
        &self,
        id: tidebreak_core::AgentRunId,
        lease_token: uuid::Uuid,
        cancel: &tidebreak_core::CancelToken,
    ) -> Result<Option<Arc<dyn ModelProvider>>> {
        if cancel.is_cancelled() {
            return Ok(None);
        }
        let resolver = self.resolver.resolve();
        tokio::pin!(resolver);
        #[cfg(test)]
        if self.config.suppress_resolver_heartbeats {
            return tokio::select! {
                biased;
                _ = cancel.cancelled() => Ok(None),
                provider = &mut resolver => Ok(Some(provider)),
            };
        }
        let mut heartbeat = tokio::time::interval(self.config.heartbeat);
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        heartbeat.tick().await;
        loop {
            if cancel.is_cancelled() {
                return Ok(None);
            }
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Ok(None),
                provider = &mut resolver => return Ok(Some(provider)),
                _ = heartbeat.tick() => {
                    if !self
                        .renew_or_validate_execution(id, lease_token, self.config.lease)
                        .await?
                    {
                        return Ok(None);
                    }
                }
            }
        }
    }

    async fn record_failure(
        &self,
        run: &AgentRun,
        lease_token: uuid::Uuid,
        error: AgentError,
    ) -> Result<SandboxAgentRunWorkerOutcome> {
        let id = run.id;
        // `fail_agent_run` owns the terminal decision — it compares the run's
        // own attempt budget under the claim lock — so the schedule's refusals
        // arrive here only as a wait that no longer matters. The deadline sweep
        // settles a run whose retry lands past its deadline.
        let delay = self
            .config
            .retry
            .delay(
                RetryAttempt {
                    id: *id.as_uuid(),
                    attempt_count: run.attempt_count,
                    max_attempts: run.max_attempts,
                    first_attempt_at: run.started_at.unwrap_or(run.created_at),
                },
                error.retry_after(),
                chrono::Utc::now(),
            )
            .unwrap_or_else(|| self.config.retry.max_delay());
        let detail = error.to_string();
        let detail = detail
            .chars()
            .take(tidebreak_core::AgentRun::MAX_ERROR_DETAIL_LEN)
            .collect::<String>();
        match self
            .store
            .fail_agent_run(
                id,
                lease_token,
                "sandbox_execution_failed",
                &detail,
                chrono_duration(delay)?,
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

    /// Park the run in `NeedsInput` with a check-in receipt for its parent.
    ///
    /// The receipt rides the terminal-submission machinery, so the parent's
    /// `wait_for_agents` consumes it like any other child outcome; unlike one,
    /// the run is paused rather than finished, and a resume grants it another
    /// cadence window.
    async fn submit_checkin(
        &self,
        run: &AgentRun,
        lease_token: uuid::Uuid,
        reason: tidebreak_core::AgentRunCheckInReason,
        steps_used: usize,
        detail: &str,
    ) -> Result<SandboxAgentRunWorkerOutcome> {
        let id = run.id;
        match self
            .store
            .submit_agent_run_checkin(
                id,
                lease_token,
                reason,
                u32::try_from(steps_used).unwrap_or(u32::MAX),
                detail,
            )
            .await?
        {
            Some(SubmitAgentRunResultOutcome::Completed(_))
            | Some(SubmitAgentRunResultOutcome::Existing(_)) => {
                self.publish_note(
                    id,
                    &format!("checkin:{}:{}", run.attempt_count, run.claim_count),
                    detail,
                )
                .await;
                // The parent may be parked on a wait this delivery satisfies.
                self.turn_wake.notify_one();
                Ok(SandboxAgentRunWorkerOutcome::CheckedIn(id))
            }
            None => {
                self.acknowledge_cancellation_or_lease_loss(id, lease_token)
                    .await
            }
        }
    }

    async fn submit_result(
        &self,
        run: &AgentRun,
        lease_token: uuid::Uuid,
        text: String,
    ) -> Result<SandboxAgentRunWorkerOutcome> {
        let id = run.id;
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

    /// Submit the run's own files as its terminal result.
    ///
    /// The files already exist as conversation outputs — the run wrote them
    /// under `output/` and the host published them by filename after the command
    /// that produced them. Submission only resolves those names to the output
    /// identities they landed on and records which ones the run offers, so no
    /// host-authored document is invented on the run's behalf.
    ///
    /// A name that does not resolve — the model named a file this run never
    /// wrote — is never silently dropped, but it also never fails the run.
    /// `done` is a terminal tool, not a checkpoint, so failing here would
    /// schedule a retry that replays byte-identical context: the model would
    /// re-emit the same wrong name until the attempt budget was gone, and any
    /// files it genuinely did produce would be discarded along with it even
    /// though they are already sitting in the outputs catalog. Instead the
    /// submission carries every name that did resolve and reports the rest in
    /// the receipt the user reads.
    async fn submit_submission(
        &self,
        run: &AgentRun,
        lease_token: uuid::Uuid,
        filenames: &[String],
        summary: &str,
    ) -> Result<SandboxAgentRunWorkerOutcome> {
        let id = run.id;
        let (outputs, unresolved) = self.resolve_submitted_outputs(run, filenames).await?;
        let summary = if unresolved.is_empty() {
            summary.to_owned()
        } else {
            let names = unresolved.join(", ");
            let mut summary = format!(
                "{summary}\n\nCould not submit {count} of the named file(s) — this run never \
                 wrote them under output/: {names}.",
                count = unresolved.len(),
            );
            summary = summary
                .chars()
                .take(tidebreak_core::MAX_SANDBOX_DONE_SUMMARY_CHARS)
                .collect();
            summary
        };
        match self
            .store
            .submit_agent_run_submission(id, lease_token, &outputs, &summary)
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

    /// Resolve submitted filenames against the outputs this run itself wrote.
    ///
    /// Matching is by filename because that is the identity the run worked with
    /// and the only one it ever sees, but a name only resolves when some
    /// revision of the output it lands on was produced by this run. Without
    /// that scope a run could hand over any file already in the conversation —
    /// including one the user's own turn produced — and have it presented as
    /// its work.
    ///
    /// The lookup asks the store for the filename rather than paging the
    /// catalog, so a conversation holding more outputs than any one page
    /// returns cannot hide a run's own file from it.
    ///
    /// Returns the outputs that resolved and, separately, the names that did
    /// not — the caller submits the former and reports the latter, rather
    /// than discarding a partially correct submission over one bad name.
    async fn resolve_submitted_outputs(
        &self,
        run: &AgentRun,
        filenames: &[String],
    ) -> Result<(Vec<AgentRunSubmittedOutput>, Vec<String>)> {
        let mut resolved = Vec::with_capacity(filenames.len());
        let mut unresolved = Vec::new();
        for filename in filenames {
            // The candidates come back newest-updated first, so the first match
            // this run wrote is the record its file landed on.
            let mut output_id = None;
            for output in self
                .store
                .find_outputs_by_filename(run.chat_id, filename)
                .await?
            {
                if self.run_produced_output(run.id, output.id).await? {
                    output_id = Some(output.id);
                    break;
                }
            }
            match output_id {
                Some(output_id) => resolved.push(AgentRunSubmittedOutput {
                    output_id,
                    filename: filename.clone(),
                }),
                None => unresolved.push(filename.clone()),
            }
        }
        Ok((resolved, unresolved))
    }

    /// Whether any revision of one output was published for this run.
    async fn run_produced_output(
        &self,
        run_id: tidebreak_core::AgentRunId,
        output_id: tidebreak_core::OutputId,
    ) -> Result<bool> {
        Ok(self
            .store
            .list_output_revisions(output_id)
            .await?
            .iter()
            .any(|revision| revision.producing_run_id == Some(run_id)))
    }

    /// Whether a run calling `done` should be handed its unfinished plan back
    /// once before the submission is accepted.
    ///
    /// A plan the run itself wrote and then abandoned mid-list is the clearest
    /// signal available that it stopped early, and the cheapest correction is
    /// to say so and let it decide. It is deliberately soft on every axis:
    ///
    /// - it is a rejected tool call, the same feedback shape a call that
    ///   arrived with company already gets, so the run reads it as an ordinary
    ///   error result and keeps its attempt;
    /// - it happens at most once per run, and the once is keyed on the reminder
    ///   itself rather than on the presence of a `done` row. A `done` row can
    ///   exist for reasons that have nothing to do with the plan — a `done`
    ///   emitted alongside a sibling is answered with `must_be_alone`, and
    ///   malformed arguments are answered too — and a model that calls tools in
    ///   parallel is exactly the population this reminder is for. Only a prior
    ///   receipt carrying [`TASK_PLAN_INCOMPLETE`] spends it;
    /// - it is withheld unless the run can afford the model step that parks the
    ///   reminder and the one that consumes it, so the reminder never converts
    ///   a run that was about to finish into a cadence failure.
    ///
    /// A run with no plan, or one whose steps are all `completed`, is never
    /// interrupted. Neither is a run that ends by simply producing final text
    /// instead of calling `done`: that path has no tool call to hand back, and
    /// inventing a synthetic one to reject would be a worse trade than missing
    /// the reminder on a run that never asked to submit anything.
    async fn done_plan_reminder(
        &self,
        run: &AgentRun,
        previous_calls: &[tidebreak_core::SandboxToolCall],
        depth: usize,
        max_steps: usize,
    ) -> Result<Option<String>> {
        if depth.saturating_add(2) > max_steps {
            return Ok(None);
        }
        for call in previous_calls
            .iter()
            .filter(|call| call.name == tidebreak_core::SANDBOX_DONE_TOOL)
        {
            let already_reminded = self
                .store
                .get_sandbox_tool_call_receipt(call.id)
                .await?
                .is_some_and(|receipt| receipt.error_code.as_deref() == Some(TASK_PLAN_INCOMPLETE));
            if already_reminded {
                return Ok(None);
            }
        }
        let Some(plan) = self.store.get_agent_run_task_plan(run.id).await? else {
            return Ok(None);
        };
        let open = tidebreak_core::open_task_plan_steps(&plan.steps);
        if open.is_empty() {
            return Ok(None);
        }
        Ok(Some(format!(
            "Your task plan still has {count} step(s) that are not completed: {steps}. Finish \
             them, or update the plan to say what you actually did, then call done again. Calling \
             done again without changing anything will submit what you have.",
            count = open.len(),
            steps = open.join("; "),
        )))
    }

    async fn submit_folder_access_proposal(
        &self,
        id: tidebreak_core::AgentRunId,
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

    /// Park one model step's tool calls as a single durable batch and release
    /// the run's lease.
    ///
    /// Every sandbox tool that needs an executor parks the same way — only the
    /// tool name differs — so they share this body rather than each carrying
    /// its own copy of the crash-recovery reasoning below. A call the host
    /// refused parks alongside them with its answer already attached, so the
    /// next step of the run reads it as an ordinary error result.
    ///
    /// A step made after the cadence withdrew the parking tools is answered
    /// rather than run: the refusal parks as a rejected row so the model can
    /// read why, spending one of the grace steps kept for exactly this — see
    /// [`SANDBOX_STEP_REFUSAL_GRACE`].
    pub(super) async fn park_sandbox_tool_calls(
        &self,
        run: AgentRun,
        lease_token: uuid::Uuid,
        intents: Vec<SandboxToolCallIntent>,
        depth: usize,
        max_steps: usize,
        narration: &str,
    ) -> Result<SandboxAgentRunWorkerOutcome> {
        if intents.is_empty() {
            // A completion that parked nothing is not a checkpoint at all;
            // there is no call to answer and nothing durable to write.
            return self
                .record_failure(
                    &run,
                    lease_token,
                    AgentError::msg("sandbox tool checkpoint has no calls to park"),
                )
                .await;
        }
        // A step past the point the cadence withdrew parking tools called a
        // tool the request no longer offered: the run wants to keep working
        // past its window. That is exactly what a check-in is for — the calls
        // are not parked (a row-less call never happened, so a resumed claim
        // simply asks the model again under the wider budget), and the run
        // pauses for its requester instead of being refused or killed.
        if depth.saturating_add(2) > max_steps {
            return self
                .submit_checkin(
                    &run,
                    lease_token,
                    tidebreak_core::AgentRunCheckInReason::StepCadence,
                    depth,
                    &format!(
                        "Used the whole {max_steps}-step window without finishing; wants to run \
                         {count} more tool call(s). Resume to grant another window, optionally \
                         with guidance, or cancel.",
                        count = intents.len(),
                    ),
                )
                .await;
        }
        let entries: Vec<SandboxToolCallParkEntry> = intents
            .into_iter()
            .map(|intent| {
                let SandboxToolCallIntent {
                    provider_id,
                    name,
                    arguments,
                    disposition,
                } = intent;
                SandboxToolCallParkEntry {
                    call: SandboxToolCallRequest {
                        id: CallId::new(),
                        agent_run_id: run.id,
                        chat_id: run.chat_id,
                        provider_id,
                        name,
                        arguments,
                    },
                    resolution: match disposition {
                        SandboxToolCallDisposition::Execute => None,
                        SandboxToolCallDisposition::Rejected {
                            error_code,
                            message,
                        } => Some(ToolCallResolution::Failed {
                            result: rejection_result(&message),
                            error_code: error_code.to_owned(),
                            error_detail: None,
                        }),
                    },
                }
            })
            .collect();
        let outcome = match self
            .store
            .park_agent_run_for_sandbox_tool_calls(run.id, lease_token, &entries)
            .await
        {
            Ok(outcome) => outcome,
            Err(error) => {
                // The local transaction may have committed before its response
                // was lost. Recover only a batch whose immutable payload and
                // producing lease match this exact model completion; never
                // issue a second model call or tool checkpoint on ambiguity.
                let mut recovered = self
                    .store
                    .list_sandbox_tool_calls_for_agent_run(run.id)
                    .await?
                    .into_iter()
                    .filter(|existing| existing.park_lease_token == lease_token)
                    .collect::<Vec<_>>();
                recovered.sort_by_key(|call| call.batch_ordinal);
                let matches = recovered.len() == entries.len()
                    && recovered.iter().zip(&entries).all(|(existing, entry)| {
                        existing.provider_id == entry.call.provider_id
                            && existing.name == entry.call.name
                            && existing.arguments == entry.call.arguments
                    });
                if matches {
                    let head = recovered[0].id;
                    self.publish_progress(run.id, head, narration).await;
                    self.wake.notify_one();
                    return Ok(SandboxAgentRunWorkerOutcome::ToolCheckpointed(head));
                }
                return Err(error);
            }
        };
        match outcome {
            ParkSandboxToolCallOutcome::Parked { calls, .. }
            | ParkSandboxToolCallOutcome::Existing { calls, .. } => {
                let head = calls
                    .first()
                    .ok_or_else(|| AgentError::msg("sandbox tool checkpoint parked no calls"))?
                    .id;
                // Keyed by the batch's first call so a replayed commit
                // republishes nothing, exactly as a single checkpoint did.
                self.publish_progress(run.id, head, narration).await;
                // This shared wake is only a latency hint; the dedicated
                // executor's durable candidate scan remains the recovery path.
                // A call the host already answered has no executor and simply
                // ignores it.
                self.wake.notify_one();
                Ok(SandboxAgentRunWorkerOutcome::ToolCheckpointed(head))
            }
            ParkSandboxToolCallOutcome::IdentityConflict => {
                self.record_failure(
                    &run,
                    lease_token,
                    AgentError::msg("sandbox tool checkpoint identity conflict"),
                )
                .await
            }
            ParkSandboxToolCallOutcome::DelegatedResourceUnavailable => {
                self.record_failure(
                    &run,
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

    /// Publish what the model said before it checkpointed, so an observer can
    /// see what the run is doing between steps.
    ///
    /// This is observation, not correctness state. It is written after the
    /// checkpoint has already committed under a proven lease, keyed by that
    /// checkpoint's durable identity so a retried commit republishes nothing,
    /// and a failure here is reported and dropped rather than allowed to
    /// disturb a transition that already succeeded.
    async fn publish_progress(
        &self,
        run_id: tidebreak_core::AgentRunId,
        call_id: CallId,
        narration: &str,
    ) {
        self.publish_note(run_id, &format!("call:{call_id}"), narration)
            .await;
    }

    /// Publish one observation under an exact idempotency key.
    async fn publish_note(&self, run_id: tidebreak_core::AgentRunId, key: &str, narration: &str) {
        if narration.trim().is_empty() {
            return;
        }
        if let Err(error) = self
            .store
            .append_agent_run_progress(run_id, key, narration)
            .await
        {
            tracing::error!("tidebreak: could not publish progress for run {run_id}: {error}");
        }
    }

    /// Record the searches the model provider ran inside a step.
    ///
    /// A vendor search is finished before this host ever sees it, so there is no
    /// checkpoint to park and nothing to execute — but the run still did the
    /// work, and its progress feed is the only place an observer can see what it
    /// did. The key is the step the search happened in rather than anything the
    /// provider chose, so a claim that replays the chain and searches again at
    /// the same depth republishes nothing.
    async fn publish_provider_executed(
        &self,
        run_id: tidebreak_core::AgentRunId,
        depth: usize,
        calls: &[ProviderExecutedCall],
    ) {
        for (index, call) in calls.iter().enumerate() {
            let text = call.progress_line();
            if let Err(error) = self
                .store
                .append_agent_run_progress(run_id, &format!("step:{depth}:tool:{index}"), &text)
                .await
            {
                tracing::error!(
                    "tidebreak: could not publish provider-executed progress for run \
                     {run_id}: {error}"
                );
            }
        }
    }

    async fn acknowledge_cancellation_or_lease_loss(
        &self,
        id: tidebreak_core::AgentRunId,
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

    /// Renew one exact in-process execution lease, resolving a monotonic
    /// heartbeat no-op against the authoritative live claim.
    ///
    /// `heartbeat_agent_run` reports whether its conditional UPDATE extended
    /// the expiry. Equal SQLite clock ticks and deadline clamping can leave a
    /// still-live exact lease unchanged, so `false` is not itself proof that
    /// execution authority was lost.
    async fn renew_or_validate_execution(
        &self,
        id: tidebreak_core::AgentRunId,
        lease_token: uuid::Uuid,
        lease: Duration,
    ) -> Result<bool> {
        if self
            .store
            .heartbeat_agent_run(id, lease_token, chrono_duration(lease)?)
            .await?
        {
            return Ok(true);
        }
        self.store
            .validate_agent_run_execution(id, lease_token, AgentRunExecutionLocation::InProcess)
            .await
    }

    fn prepare_private_scratch(&self, id: tidebreak_core::AgentRunId) -> Result<()> {
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
