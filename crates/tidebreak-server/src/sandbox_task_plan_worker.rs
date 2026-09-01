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

use tidebreak_core::{
    parse_update_task_plan_arguments, task_plan_summary, AgentError, ClaimSandboxToolCallOutcome,
    ResolveSandboxToolCallOutcome, Result, SandboxToolCall, Store, ToolCallResolution,
    UPDATE_TASK_PLAN_TOOL,
};
use tokio::sync::Notify;

use crate::lane::{self, LaneOutcome, LanePacing, LaneStep};
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
    Resolved(tidebreak_core::CallId),
    LeaseLost(tidebreak_core::CallId),
}

impl LaneOutcome for SandboxTaskPlanWorkerOutcome {
    fn lane_step(&self) -> LaneStep {
        match self {
            Self::Idle => LaneStep::Idle,
            _ => LaneStep::Worked,
        }
    }
}

/// The name this worker's lanes log under.
const LANE_NAME: &str = "sandbox task-plan worker";

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
        lane::supervise_lanes(
            LANE_NAME,
            self.config.max_concurrency,
            LaneBackoff::new(self.config.failure_delay, self.config.failure_delay_cap),
            move || self.clone().run_lane(),
        )
        .await;
    }

    async fn run_lane(self) {
        let this = &self;
        lane::run_lane(LANE_NAME, self.pacing(), &self.wake, move || {
            this.run_once()
        })
        .await;
    }

    fn pacing(&self) -> LanePacing {
        LanePacing::backoff(
            self.config.idle_min,
            self.config.idle_cap,
            self.config.failure_delay,
            self.config.failure_delay_cap,
        )
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
