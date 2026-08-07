//! Durable execution of a background run's `update_task_plan` checkpoint.
//!
//! A sandbox agent's plan row lives in the host's database, so the call cannot
//! be answered where the agent runs. It takes the same shape every other
//! sandbox tool takes: the run parks a checkpoint and releases its lease, this
//! lane claims that checkpoint under its own exact executor lease, and the
//! run resumes from the immutable receipt.
//!
//! Unlike the search and exec lanes there is nothing here to time out, retry,
//! or cancel mid-flight: the work is one local transaction that both records
//! the plan and writes the receipt, so a claim either commits both or neither.

use std::sync::Arc;
use std::time::Duration;

use openwave_core::{
    parse_update_task_plan_arguments, task_plan_summary, AgentError, ClaimSandboxToolCallOutcome,
    ResolveSandboxToolCallOutcome, Result, SandboxToolCall, Store, ToolCallResolution,
    UPDATE_TASK_PLAN_TOOL,
};
use tokio::sync::Notify;

use crate::retry::LaneBackoff;

const CANDIDATE_BATCH_SIZE: u64 = 16;

#[derive(Debug, Clone, Copy)]
pub(crate) struct SandboxTaskPlanWorkerConfig {
    /// Short by design: the whole operation is one local transaction, so a
    /// lease long enough to survive scheduling is already generous.
    lease: Duration,
    idle_min: Duration,
    idle_cap: Duration,
    failure_delay: Duration,
    failure_delay_cap: Duration,
    max_concurrency: usize,
}

impl Default for SandboxTaskPlanWorkerConfig {
    fn default() -> Self {
        Self {
            lease: Duration::from_secs(30),
            idle_min: Duration::from_millis(250),
            idle_cap: Duration::from_secs(5),
            failure_delay: Duration::from_secs(1),
            failure_delay_cap: Duration::from_secs(30),
            max_concurrency: 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SandboxTaskPlanWorkerOutcome {
    Idle,
    Resolved(openwave_core::CallId),
    LeaseLost(openwave_core::CallId),
}

#[derive(Clone)]
pub(crate) struct SandboxTaskPlanWorker {
    store: Arc<dyn Store>,
    wake: Arc<Notify>,
    config: SandboxTaskPlanWorkerConfig,
}

impl SandboxTaskPlanWorker {
    pub(crate) fn new(
        store: Arc<dyn Store>,
        wake: Arc<Notify>,
        config: SandboxTaskPlanWorkerConfig,
    ) -> Self {
        assert!(!config.lease.is_zero());
        assert!(config.max_concurrency > 0);
        Self {
            store,
            wake,
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
                eprintln!("openwave: sandbox task-plan worker lane stopped: {error}");
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
                Ok(SandboxTaskPlanWorkerOutcome::Idle) => {
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
                    eprintln!("openwave: sandbox task-plan worker iteration failed: {error}");
                    let delay = failure_backoff.next_delay();
                    tokio::select! {
                        _ = tokio::time::sleep(delay) => {}
                        _ = self.wake.notified() => {}
                    }
                }
            }
        }
    }

    /// Claim and resolve one exact persisted plan checkpoint.
    pub(crate) async fn run_once(&self) -> Result<SandboxTaskPlanWorkerOutcome> {
        for candidate in self
            .store
            .list_sandbox_tool_call_candidates_named(UPDATE_TASK_PLAN_TOOL, CANDIDATE_BATCH_SIZE)
            .await?
        {
            let lease_token = uuid::Uuid::new_v4();
            let call = match self
                .store
                .claim_sandbox_tool_call_named(
                    candidate.id,
                    UPDATE_TASK_PLAN_TOOL,
                    lease_token,
                    chrono_duration(self.config.lease)?,
                )
                .await?
            {
                ClaimSandboxToolCallOutcome::Claimed(call)
                | ClaimSandboxToolCallOutcome::Existing(call) => call,
                ClaimSandboxToolCallOutcome::Unavailable => continue,
            };
            self.wake.notify_one();
            return self.process(call, lease_token).await;
        }
        Ok(SandboxTaskPlanWorkerOutcome::Idle)
    }

    async fn process(
        &self,
        call: SandboxToolCall,
        lease_token: uuid::Uuid,
    ) -> Result<SandboxTaskPlanWorkerOutcome> {
        // The model step already validated these arguments before parking the
        // checkpoint, so a failure here is a row nothing should have written.
        // It still answers rather than wedges: the run reads the correction and
        // carries on from the same attempt.
        let outcome = match parse_update_task_plan_arguments(&call.arguments) {
            Ok(arguments) => {
                let resolution = ToolCallResolution::Completed {
                    result: task_plan_summary(&arguments.steps),
                };
                self.store
                    .resolve_sandbox_task_plan_call(
                        call.id,
                        lease_token,
                        &arguments.steps,
                        &resolution,
                    )
                    .await?
            }
            Err(correction) => {
                self.store
                    .resolve_sandbox_tool_call(
                        call.id,
                        lease_token,
                        &ToolCallResolution::Failed {
                            result: correction,
                            error_code: "invalid_arguments".into(),
                            error_detail: None,
                        },
                    )
                    .await?
            }
        };
        match outcome {
            ResolveSandboxToolCallOutcome::Resolved | ResolveSandboxToolCallOutcome::Existing => {
                self.wake.notify_one();
                Ok(SandboxTaskPlanWorkerOutcome::Resolved(call.id))
            }
            ResolveSandboxToolCallOutcome::NotFound
            | ResolveSandboxToolCallOutcome::AlreadyTerminal
            | ResolveSandboxToolCallOutcome::LeaseLost => {
                Ok(SandboxTaskPlanWorkerOutcome::LeaseLost(call.id))
            }
        }
    }
}

fn chrono_duration(duration: Duration) -> Result<chrono::Duration> {
    chrono::Duration::from_std(duration)
        .map_err(|error| AgentError::msg(format!("invalid sandbox task-plan duration: {error}")))
}
