//! Foreground tools for supervising delegated background agents.
//!
//! The parent model already *sees* a check-in — the child's pause settles its
//! `wait_for_agents` with a typed [`tidebreak_core::AgentRunResultPayload::CheckIn`]
//! payload — but until these tools it could only describe the situation to
//! the user. `resume_agent` and `cancel_agent` close that loop: the same
//! transitions the run panel's buttons drive, callable by the model that
//! spawned the child.
//!
//! Both are ordinary synchronous server tools rather than orchestration
//! checkpoints: nothing here parks the turn, and both effects are idempotent
//! store transitions fenced elsewhere (the cancellation state machine and the
//! resume's status/updated-at filters).

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use tidebreak_core::{
    AgentError, AgentRunId, AgentRunStatus, AgentRunTier, ApprovalClass, Result,
    SandboxToolCallStatus, Store, Tool, ToolCtx, ToolErrorCategory, ToolOutput, ToolSpec,
};
use tokio::sync::Notify;
use uuid::Uuid;

use crate::state::{SandboxAttemptGuard, SandboxSteerGuard};

/// Stable name for the foreground child-resume tool.
pub const RESUME_AGENT_TOOL: &str = "resume_agent";
/// Stable name for the foreground child-cancel tool.
pub const CANCEL_AGENT_TOOL: &str = "cancel_agent";

/// Process-local acceleration after a durable sandbox cancellation commits.
///
/// The store's immutable cancellation receipts are the source of exact model
/// and checkpoint identities. The optional lease supplied by the committing
/// caller closes the smaller cancel-before-receipt-read window for an attached
/// container drive. Every signal is only a latency hint: durable cancellation
/// and terminal-write fencing remain authoritative.
#[derive(Clone)]
pub struct SandboxCancellationAcceleration {
    store: Arc<dyn Store>,
    attempts: Arc<SandboxAttemptGuard>,
    steering: Arc<SandboxSteerGuard>,
    worker_wake: Arc<Notify>,
}

impl SandboxCancellationAcceleration {
    pub fn new(
        store: Arc<dyn Store>,
        attempts: Arc<SandboxAttemptGuard>,
        steering: Arc<SandboxSteerGuard>,
        worker_wake: Arc<Notify>,
    ) -> Self {
        Self {
            store,
            attempts,
            steering,
            worker_wake,
        }
    }

    fn detached(store: Arc<dyn Store>) -> Self {
        Self::new(
            store,
            Arc::new(SandboxAttemptGuard::default()),
            Arc::new(SandboxSteerGuard::default()),
            Arc::new(Notify::new()),
        )
    }

    /// Signal the exact local owners named by a committed cancellation.
    pub async fn signal_after_commit(
        &self,
        run_id: AgentRunId,
        committed_lease_token: Option<Uuid>,
    ) {
        // A live cancellation outcome returns the exact durable run lease.
        // Signal its container drive before any further store read so reverse
        // provider admission cannot continue until the next heartbeat.
        if let Some(lease_token) = committed_lease_token {
            self.steering.cancel_container_drive(run_id, lease_token);
        }

        if let Ok(Some(signal)) = self.store.get_agent_run_cancellation_signal(run_id).await {
            self.steering
                .cancel_container_drive(run_id, signal.lease_token);
            self.attempts.cancel_model(run_id, signal.lease_token);
        }

        // Cancelling a waiting run atomically terminalizes its live tool call
        // and records the exact executor lease. Never infer that lease from
        // mutable call state or signal every call belonging to a run.
        if let Ok(calls) = self
            .store
            .list_sandbox_tool_calls_for_agent_run(run_id)
            .await
        {
            for call in calls {
                if call.status != SandboxToolCallStatus::Cancelled {
                    continue;
                }
                if let Ok(Some(receipt)) = self.store.get_sandbox_tool_call_receipt(call.id).await {
                    if receipt.status == SandboxToolCallStatus::Cancelled {
                        self.attempts.cancel_checkpoint(
                            call.id,
                            run_id,
                            receipt.executor_lease_token,
                        );
                    }
                }
            }
        }

        // A missed/coalesced wake is harmless: the worker's durable claim scan
        // is authoritative. Waking here avoids waiting for its polling floor.
        self.worker_wake.notify_one();
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResumeAgentArgs {
    agent_id: AgentRunId,
    #[serde(default)]
    guidance: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CancelAgentArgs {
    agent_id: AgentRunId,
    #[serde(default)]
    reason: Option<String>,
}

/// The target named by a supervision call, or the refusal to hand the model.
async fn own_background_child(
    store: &Arc<dyn Store>,
    ctx: &ToolCtx,
    agent_id: AgentRunId,
) -> Result<std::result::Result<tidebreak_core::AgentRun, ToolOutput>> {
    let Some(run) = store.get_agent_run(agent_id).await? else {
        return Ok(Err(ToolOutput::failed(
            ToolErrorCategory::InvalidArguments,
            format!("agent {agent_id} does not exist"),
        )));
    };
    // Same boundary as the HTTP routes: a conversation supervises only the
    // background children it owns. Wrong-chat targets read as absent so a
    // hallucinated id cannot probe another conversation's work.
    if run.chat_id != ctx.chat_id || run.tier != AgentRunTier::Background {
        return Ok(Err(ToolOutput::failed(
            ToolErrorCategory::InvalidArguments,
            format!("agent {agent_id} does not exist"),
        )));
    }
    Ok(Ok(run))
}

/// Resume a background child paused at a check-in, optionally with guidance.
pub struct ResumeAgentTool {
    store: Arc<dyn Store>,
}

impl ResumeAgentTool {
    pub fn new(store: Arc<dyn Store>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for ResumeAgentTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: RESUME_AGENT_TOOL.into(),
            description: "Resume one of this conversation's background agents that paused to \
                          check in (status needs_input), granting it another step window. \
                          Optional guidance is folded into the agent's task before it \
                          continues — use it to redirect or narrow the work. Only an agent \
                          spawned by this conversation can be resumed."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "agent_id": {
                        "type": "string",
                        "description": "The paused agent's id, from wait_for_agents' check_in result."
                    },
                    "guidance": {
                        "type": "string",
                        "description": "Optional direction for the agent before it continues."
                    }
                },
                "required": ["agent_id"],
                "additionalProperties": false
            }),
        }
    }

    /// `ReadOnly` deliberately: resuming grants no reach the spawn did not
    /// already carry — the child keeps exactly the sandbox capabilities it was
    /// admitted with, and the spawn itself is the `Sensitive` boundary that
    /// carried their weight. Gating the continuation would make supervising
    /// delegated work more interrupting than delegating it.
    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::ReadOnly
    }

    async fn execute(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let args = match serde_json::from_value::<ResumeAgentArgs>(args) {
            Ok(args) => args,
            Err(_) => {
                return Ok(ToolOutput::failed(
                    ToolErrorCategory::InvalidArguments,
                    "expected {\"agent_id\": \"...\", \"guidance\"?: \"...\"}",
                ));
            }
        };
        let run = match own_background_child(&self.store, ctx, args.agent_id).await? {
            Ok(run) => run,
            Err(refusal) => return Ok(refusal),
        };
        let guidance = args
            .guidance
            .as_deref()
            .map(str::trim)
            .filter(|guidance| !guidance.is_empty());
        match self
            .store
            .resume_agent_run_from_checkin(run.id, guidance)
            .await
        {
            Ok(Some(resumed)) => Ok(ToolOutput::text(format!(
                "Resumed agent {id}; it has another step window{with}. Call wait_for_agents \
                 to collect its next result or check-in.",
                id = resumed.id,
                with = if guidance.is_some() {
                    " and your guidance"
                } else {
                    ""
                },
            ))),
            Ok(None) => Ok(ToolOutput::failed(
                ToolErrorCategory::ToolFailed,
                format!(
                    "agent {id} is not paused at a check-in (status: {status})",
                    id = run.id,
                    status = run.status.as_str(),
                ),
            )),
            Err(AgentError::Store(message)) => Ok(ToolOutput::failed(
                ToolErrorCategory::InvalidArguments,
                message.to_string(),
            )),
            Err(error) => Err(error),
        }
    }
}

/// Cancel one of the conversation's background children.
pub struct CancelAgentTool {
    store: Arc<dyn Store>,
    cancellation_acceleration: SandboxCancellationAcceleration,
}

impl CancelAgentTool {
    pub fn new(store: Arc<dyn Store>) -> Self {
        Self {
            cancellation_acceleration: SandboxCancellationAcceleration::detached(store.clone()),
            store,
        }
    }

    pub fn with_cancellation_acceleration(
        mut self,
        cancellation_acceleration: SandboxCancellationAcceleration,
    ) -> Self {
        self.cancellation_acceleration = cancellation_acceleration;
        self
    }
}

#[async_trait]
impl Tool for CancelAgentTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: CANCEL_AGENT_TOOL.into(),
            description: "Stop one of this conversation's background agents. Use it on an \
                          agent whose check-in shows the work is no longer needed or is not \
                          converging, or any agent that should stop. Cancellation is durable \
                          and settles any pending wait on the agent with a cancelled result. \
                          Only an agent spawned by this conversation can be cancelled."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "agent_id": {
                        "type": "string",
                        "description": "The agent to stop."
                    },
                    "reason": {
                        "type": "string",
                        "description": "Optional short reason, recorded in the agent's progress feed."
                    }
                },
                "required": ["agent_id"],
                "additionalProperties": false
            }),
        }
    }

    /// `ReadOnly` for the same reason as resume: stopping the conversation's
    /// own delegated work reaches nothing beyond that work. The cost of a
    /// wrong cancel is lost child progress, which the transcript records —
    /// not an effect on anything the user owns outside the chat.
    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::ReadOnly
    }

    async fn execute(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let args = match serde_json::from_value::<CancelAgentArgs>(args) {
            Ok(args) => args,
            Err(_) => {
                return Ok(ToolOutput::failed(
                    ToolErrorCategory::InvalidArguments,
                    "expected {\"agent_id\": \"...\", \"reason\"?: \"...\"}",
                ));
            }
        };
        let run = match own_background_child(&self.store, ctx, args.agent_id).await? {
            Ok(run) => run,
            Err(refusal) => return Ok(refusal),
        };
        if run.status.is_terminal() {
            if run.status == AgentRunStatus::Cancelled {
                self.cancellation_acceleration
                    .signal_after_commit(run.id, run.lease_token)
                    .await;
            }
            return Ok(ToolOutput::text(format!(
                "Agent {id} already finished (status: {status}); nothing to cancel.",
                id = run.id,
                status = run.status.as_str(),
            )));
        }
        if let Some(reason) = args
            .reason
            .as_deref()
            .map(str::trim)
            .filter(|reason| !reason.is_empty())
        {
            // Best-effort observation, keyed by the cancelling call so a
            // retried tool execution republishes nothing.
            let key = ctx
                .call_id
                .map(|call| format!("cancel_reason:{call}"))
                .unwrap_or_else(|| format!("cancel_reason:{}", run.claim_count));
            let reason: String = reason.chars().take(1_000).collect();
            let _ = self
                .store
                .append_agent_run_progress(
                    run.id,
                    &key,
                    &format!("Cancelled by the chat: {reason}"),
                )
                .await;
        }
        // The request is serialized against live leases; a busy run flips to
        // cancelling and quiesces, a parked one terminalizes immediately. A
        // handful of retries covers claim races the same way the HTTP route's
        // loop does.
        for _ in 0..8 {
            if let Some(outcome) = self.store.request_agent_run_cancellation(run.id).await? {
                let committed = match &outcome {
                    tidebreak_core::RequestAgentRunCancellationOutcome::Cancelled(run)
                    | tidebreak_core::RequestAgentRunCancellationOutcome::Requested(run)
                    | tidebreak_core::RequestAgentRunCancellationOutcome::Existing(run)
                    | tidebreak_core::RequestAgentRunCancellationOutcome::AlreadyTerminal(run) => {
                        run
                    }
                };
                self.cancellation_acceleration
                    .signal_after_commit(committed.id, committed.lease_token)
                    .await;
                let cancelled = matches!(
                    outcome,
                    tidebreak_core::RequestAgentRunCancellationOutcome::Cancelled(_)
                );
                return Ok(ToolOutput::text(if cancelled {
                    format!("Cancelled agent {id}.", id = run.id)
                } else {
                    format!(
                        "Agent {id} is stopping; a pending wait on it settles with a \
                         cancelled result.",
                        id = run.id
                    )
                }));
            }
            let Some(current) = self.store.get_agent_run(run.id).await? else {
                break;
            };
            if current.status == AgentRunStatus::Cancelled {
                self.cancellation_acceleration
                    .signal_after_commit(current.id, current.lease_token)
                    .await;
                return Ok(ToolOutput::text(format!(
                    "Cancelled agent {id}.",
                    id = run.id
                )));
            }
            tokio::task::yield_now().await;
        }
        Ok(ToolOutput::failed(
            ToolErrorCategory::ToolFailed,
            format!(
                "cancellation of agent {id} could not be serialized; try again",
                id = run.id
            ),
        ))
    }
}
