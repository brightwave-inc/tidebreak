//! Driving a sandbox-resident agent run inside a local container, host-driven
//! and attached-only (issue #874).
//!
//! This is the host side of the sandbox-resident execution location. Where the
//! [in-process worker](crate::sandbox_agent_run_worker) advances a background run
//! by streaming the model itself, this driver hands the loop to a container and
//! becomes the container's model proxy over the reverse channel. Concretely, for
//! one admitted `container`-located run it:
//!
//! 1. **claims** the run under a bounded lease
//!    ([`Store::claim_container_agent_run`]) — the same lease that fences the
//!    result commit at the end;
//! 2. **provisions** a container from the agent image through the
//!    [`SandboxBackend`], stamping a host-minted correlation tag so the backend's
//!    orphan sweep can reclaim a container whose run never committed a handle;
//! 3. **connects** to the container's mapped loopback address with
//!    [`WireClient::connect`], doing the version handshake;
//! 4. **drives** the run: it backs the protocol host's operation log with the
//!    crash-safe [`DurableOperationStore`], answers the sandbox's reverse-RPC
//!    model-inference calls with the host's own [`ProviderResolver`] (the same
//!    resolver the in-process worker uses — no model credential lives in the
//!    container), drains the event stream committing its cursor, and commits the
//!    agent's final result through the run tier's fenced result path;
//! 5. **tears down** the container idempotently on the run's terminal state.
//!
//! Attached-only: a local container runtime has no lifetime cap it can enforce
//! from outside the container, so the run may not work while unattached and this
//! driver is the container's only model path.
//!
//! # Multi-thread precondition
//!
//! [`DurableOperationStore`]'s synchronous [`OperationStore`] bridge drives the
//! async store through [`tokio::task::block_in_place`], which requires a
//! multi-thread runtime. The host runs on one; every test here uses the
//! multi-thread flavor.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use tidebreak_core::storage::RecordAgentRunModelStepOutcome;
use tidebreak_core::{
    AgentConfig, AgentError, AgentRun, AgentRunExecutionLocation, AgentRunId, AgentRunStatus,
    BeginSandboxProvisionOutcome, CancelToken, ChatMessage, ChatRequest, ProviderEvent, Result,
    Role, SandboxAdmissionMode, SandboxProvisionState, Store, SubmitAgentRunResultOutcome, Usage,
};
use tidebreak_sandbox_protocol::{
    events::EventPayload,
    ids::{EventCursor, RunId},
    init::{AdmissionMode, PolicySnapshot, RunInit, ScopedModelToken},
    protocol::{ErrorCode, ErrorResponse, Response, PROTOCOL_VERSION},
    reverse::{
        Capability, CapabilityResponder, GrantSet, ModelInferenceResult, ReverseRequest,
        ReverseResult, RunProvenance,
    },
    steer::SteerMessage,
    AttachRequest, BackendError, CapabilityHost, ConnectError, ProvisionRequest, SandboxAddress,
    SandboxBackend, SandboxHandle, SandboxNetworkPolicy, SandboxTag, WireClient,
};
use tokio::net::TcpStream;
use uuid::Uuid;

use crate::durable_oplog::DurableOperationStore;
use crate::resolver::ProviderResolver;
use crate::sandbox_admission::{evaluate_detached_admission, DetachedPreconditions};
use crate::sandbox_docker::DEFAULT_IDLE_TIMEOUT_SECS;
use crate::scoped_model_token::{GatewayScopedTokenIssuer, ScopedModelTokenIssuer};
use crate::state::SandboxSteerGuard;

/// The provider attribution stamped on reverse operations from a local
/// container. Untrusted attribution rendered on consent prompts, never a claim
/// the container makes about itself.
const CONTAINER_PROVENANCE_PROVIDER: &str = "local-container";
/// The image's configured idle timeout and the host cadence that must remain
/// comfortably below it. Kept together so the safety margin is reviewable and
/// testable instead of existing only in matching comments across crates.
const SANDBOX_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);

/// Final accounting and terminal writes are exact, idempotent CAS operations,
/// so transient store failures can be retried without creating another model
/// step or changing the terminal payload. Keep retries responsive while still
/// backing off under a longer outage; the run's absolute deadline is the hard
/// bound enforced below.
const TERMINAL_RETRY_INITIAL: Duration = Duration::from_millis(10);
const TERMINAL_RETRY_MAX: Duration = Duration::from_millis(250);

/// How many steering instructions the host holds for one attached run before it
/// refuses new ones. Steering is applied at the sandbox's step boundary, so a
/// small backlog covers a burst arriving mid-step; past it the caller is told to
/// retry rather than the host growing an unbounded queue for a run that may
/// never read it.
const STEER_BACKLOG: usize = 8;

const _: () = assert!(
    SANDBOX_KEEPALIVE_INTERVAL.as_secs() * 2 < DEFAULT_IDLE_TIMEOUT_SECS,
    "two missed keepalive intervals must still leave time before idle expiry"
);

/// Tunables for driving one sandbox-resident container run.
#[derive(Debug, Clone, Copy)]
pub struct SandboxContainerRunConfig {
    /// The bounded lease the driver claims the run under; the same lease fences
    /// the result commit.
    pub lease: Duration,
    /// How often the driver extends that lease while the container works.
    ///
    /// A container run outlives one lease period, and the in-process reaper
    /// terminalizes a background run whose lease expires. Must be well under
    /// [`lease`](Self::lease).
    pub heartbeat: Duration,
    /// How often a container drive re-reads its exact durable execution fence
    /// while setup or attached work is in flight.
    ///
    /// Process-local cancellation is only an acceleration: another server can
    /// commit the same immutable cancellation receipt without sharing this
    /// process's token. This shorter cadence observes that durable transition
    /// promptly during model resolution, provisioning, attachment, and reverse
    /// provider work instead of waiting for the ordinary lease heartbeat.
    pub durable_fence_interval: Duration,
    /// How long to wait for one TCP dial of the container's loopback address.
    pub dial_timeout: Duration,
    /// How many times the driver re-dials after an unplanned disconnect before
    /// giving up on an attached-only run. Reattachment resumes the event stream
    /// from the committed cursor and replays any recorded reverse answer.
    pub reattach_attempts: u32,
    /// Backoff between re-dials.
    pub reattach_backoff: Duration,
    /// How long the backend's create call may take before the run's durable
    /// provisioning intent lapses.
    ///
    /// Recovery is driven by the intent, not by what the provider reports: a
    /// crash on either side of the create converges through the lapse and the
    /// tag sweep, and a handle commit that arrives after the lapse finds the
    /// record already disowned and destroys its own container.
    pub provision_window: Duration,
    /// How many container runs may be `running` at once.
    ///
    /// Container runs bypass the in-process scheduler's global and per-chat
    /// caps, so the claim enforces this bound instead; a run refused by it
    /// stays queued for a later pass.
    pub max_concurrent_containers: u32,
    /// How many model-inference operations the host will answer for one run.
    ///
    /// The sandbox's own step limit runs inside the untrusted container, so it
    /// bounds nothing; this cap is what actually stops a compromised or buggy
    /// sandbox from spending the user's model credentials indefinitely. Every
    /// call that reaches the host's provider spends one unit — including a call
    /// that then fails retryably — while a re-issue answered from the recorded
    /// operation log spends nothing. Exhaustion is refused non-retryably, which
    /// fails the sandbox's model step and terminalizes the run.
    pub max_inference_operations: u32,
}

impl Default for SandboxContainerRunConfig {
    fn default() -> Self {
        Self {
            lease: Duration::from_secs(60),
            heartbeat: Duration::from_secs(15),
            durable_fence_interval: Duration::from_millis(250),
            dial_timeout: Duration::from_secs(10),
            reattach_attempts: 5,
            reattach_backoff: Duration::from_millis(250),
            // Ample for `docker run` against a present image; an image pull on
            // a cold machine is paid at build/install time, not here.
            provision_window: Duration::from_secs(120),
            // Containers are heavier than in-process runs (a full agent image
            // each); a small bound keeps a burst of spawns from exhausting the
            // local machine.
            max_concurrent_containers: 4,
            // Three times the in-container loop's own step limit: a well-behaved
            // run never approaches it even with retried provider failures, and a
            // hostile one is cut off within one order of magnitude of legitimate
            // spend.
            max_inference_operations: 24,
        }
    }
}

/// How a driven sandbox-resident container run finished.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxContainerRunOutcome {
    /// The container submitted a final result and the host committed it.
    Completed(AgentRunId),
    /// The result was already committed by this attempt; the redelivery was
    /// acknowledged with the original receipt.
    AlreadyCompleted(AgentRunId),
    /// The run failed terminally (no result within its bounds); a sandbox-
    /// resident run is never re-executed.
    Failed(AgentRunId),
    /// The run was cancelled while the driver drove it; the driver committed
    /// the terminal cancellation and tore the container down.
    Cancelled(AgentRunId),
    /// The lease was lost or the run was already terminal when the driver tried
    /// to commit — nothing was committed by this attempt.
    LeaseLost(AgentRunId),
}

/// The host's model proxy for an attached-only sandbox run.
///
/// It answers each reverse [`ReverseRequest::ModelInference`] with the host's own
/// configured model access — the same [`ProviderResolver`] the in-process worker
/// resolves — so no model credential ever enters the container. The
/// [`CapabilityHost`] calls this at most once per [`OperationId`], recording the
/// bounded completion in the [`DurableOperationStore`]; a re-issue after a
/// reconnect replays that record rather than spending a second time.
struct HostModelProxy {
    resolver: Arc<dyn ProviderResolver>,
    /// Shared with the exact claimed container drive. A durable cancellation
    /// trips it before the route acknowledges, fencing a reverse request that
    /// races the outer drive's connection teardown from starting provider
    /// egress.
    cancel: CancelToken,
    /// Exact durable execution authority checked immediately before resolver
    /// and provider egress. The outer drive also polls this fence, but the
    /// responder revalidates at the actual credential boundary so a remote
    /// cancellation observed between polls fails closed.
    lease_guard: Option<HostModelLeaseGuard>,
    /// The run's model selection already resolved through the host's model
    /// registry (provider route, reasoning shape, token and effort bounds), so
    /// every proxied completion egresses under exactly the policy an in-process
    /// run would. Resolved once at attach and failed closed there, never
    /// re-derived per request from an untrusted prompt.
    config: AgentConfig,
    /// Model-inference operations answered so far, against
    /// [`SandboxContainerRunConfig::max_inference_operations`]. One proxy lives
    /// for the whole drive, so the count survives reattaches; replays from the
    /// operation log never reach this responder and spend nothing.
    spent: AtomicU32,
    /// The per-run cap the count is checked against.
    budget: u32,
    /// Cumulative durable baseline for the next provider operation. Serialized
    /// because the reverse request lane may deliver concurrent operations,
    /// while accounting is one ordered sequence per run.
    accounting: Option<HostModelAccounting>,
    /// Provider steps that have emitted a billable/completion-bearing event but
    /// have not yet reached the durable accounting CAS. Detached reverse-RPC
    /// tasks can outlive their connection, so the container driver drains this
    /// set after cancelling those tasks and before terminalizing the run.
    observed: HostModelObservedAccounting,
}

struct HostModelAccounting {
    store: Arc<dyn Store>,
    run_id: AgentRunId,
    lease_token: Uuid,
    baseline: tokio::sync::Mutex<(i32, Usage)>,
}

struct HostModelLeaseGuard {
    store: Arc<dyn Store>,
    run_id: AgentRunId,
    lease_token: Uuid,
}

impl HostModelLeaseGuard {
    async fn authorize_egress(&self, cancel: &CancelToken) -> Result<bool> {
        if cancel.is_cancelled() {
            return Ok(false);
        }
        let live = self
            .store
            .validate_agent_run_execution(
                self.run_id,
                self.lease_token,
                AgentRunExecutionLocation::Container,
            )
            .await?;
        if !live {
            cancel.cancel();
            return Ok(false);
        }
        Ok(!cancel.is_cancelled())
    }
}

#[derive(Default)]
struct HostModelObservedAccounting {
    next_id: AtomicU64,
    pending: Mutex<BTreeMap<u64, Usage>>,
}

impl HostModelObservedAccounting {
    fn mark(&self, id: &mut Option<u64>) -> u64 {
        *id.get_or_insert_with(|| {
            let id = self.next_id.fetch_add(1, Ordering::SeqCst);
            self.pending
                .lock()
                .expect("host model observed-accounting lock")
                .insert(id, Usage::default());
            id
        })
    }

    fn add_usage(&self, id: &mut Option<u64>, reported: Usage) -> Result<()> {
        let id = self.mark(id);
        let mut pending = self
            .pending
            .lock()
            .expect("host model observed-accounting lock");
        let usage = pending
            .get(&id)
            .copied()
            .unwrap_or_default()
            .checked_add(reported)
            .ok_or_else(|| AgentError::msg("host model usage exceeded the supported total"))?;
        pending.insert(id, usage);
        Ok(())
    }

    fn usage(&self, id: u64) -> Option<Usage> {
        self.pending
            .lock()
            .expect("host model observed-accounting lock")
            .get(&id)
            .copied()
    }

    fn remove(&self, id: u64) {
        self.pending
            .lock()
            .expect("host model observed-accounting lock")
            .remove(&id);
    }

    fn first(&self) -> Option<u64> {
        self.pending
            .lock()
            .expect("host model observed-accounting lock")
            .keys()
            .next()
            .copied()
    }
}

impl HostModelProxy {
    fn cancelled_response() -> Response<ReverseResult> {
        Response::Error(ErrorResponse::new(
            ErrorCode::Cancelled,
            "the container run was cancelled",
            false,
        ))
    }

    async fn durable_egress_authorized(&self) -> std::result::Result<bool, ()> {
        let Some(guard) = &self.lease_guard else {
            return Ok(!self.cancel.is_cancelled());
        };
        guard.authorize_egress(&self.cancel).await.map_err(|error| {
            tracing::error!(
                "tidebreak: host model proxy could not revalidate its execution fence: {error}"
            );
        })
    }

    async fn account_model_step(&self, usage: Usage) -> Result<()> {
        let Some(accounting) = &self.accounting else {
            return Ok(());
        };
        let mut baseline = accounting.baseline.lock().await;
        let (model_steps, cumulative_usage) = *baseline;
        let next_steps = model_steps
            .checked_add(1)
            .ok_or_else(|| AgentError::msg("container model-step total overflowed"))?;
        let next_usage = cumulative_usage
            .checked_add(usage)
            .ok_or_else(|| AgentError::msg("container model usage total overflowed"))?;
        let outcome = self
            .accounting_store(accounting)
            .record_agent_run_model_step(
                accounting.run_id,
                accounting.lease_token,
                model_steps,
                cumulative_usage,
                usage,
            )
            .await;
        match outcome {
            Ok(RecordAgentRunModelStepOutcome::Recorded(run))
            | Ok(RecordAgentRunModelStepOutcome::Existing(run)) => {
                *baseline = (run.model_steps, run.usage);
                Ok(())
            }
            Ok(RecordAgentRunModelStepOutcome::IdentityConflict(run)) => Err(AgentError::msg(
                format!(
                    "container model-step accounting conflicted: expected step {next_steps}, found {}",
                    run.model_steps
                ),
            )),
            Ok(RecordAgentRunModelStepOutcome::LeaseLost) => {
                Err(AgentError::msg("container agent-run lease was lost"))
            }
            Err(error) => {
                let recovered = accounting
                    .store
                    .get_agent_run(accounting.run_id)
                    .await?
                    .is_some_and(|run| run.model_steps == next_steps && run.usage == next_usage);
                if recovered {
                    *baseline = (next_steps, next_usage);
                    Ok(())
                } else {
                    Err(error)
                }
            }
        }
    }

    fn accounting_store<'a>(&self, accounting: &'a HostModelAccounting) -> &'a dyn Store {
        &*accounting.store
    }

    async fn account_observation(&self, id: u64) -> Result<()> {
        let Some(usage) = self.observed.usage(id) else {
            return Ok(());
        };
        self.account_model_step(usage).await?;
        self.observed.remove(id);
        Ok(())
    }

    async fn flush_observed_accounting(&self) -> Result<()> {
        while let Some(id) = self.observed.first() {
            self.account_observation(id).await?;
        }
        Ok(())
    }
}

#[async_trait]
impl CapabilityResponder for HostModelProxy {
    async fn respond(&self, request: ReverseRequest) -> Response<ReverseResult> {
        let ReverseRequest::ModelInference(params) = request else {
            // The grant set only authorizes model inference, so a well-behaved
            // sandbox never reaches this; refuse an unknown capability rather
            // than trust it.
            return Response::Error(ErrorResponse::new(
                ErrorCode::InvalidRequest,
                "unsupported reverse capability",
                false,
            ));
        };
        if self.cancel.is_cancelled() {
            return Self::cancelled_response();
        }
        match self.durable_egress_authorized().await {
            Ok(true) => {}
            Ok(false) => return Self::cancelled_response(),
            Err(()) => {
                return Response::Error(ErrorResponse::new(
                    ErrorCode::Internal,
                    "host model execution authority could not be verified",
                    true,
                ));
            }
        }
        // Spend before resolving: a call that fails retryably still consumed a
        // provider attempt, and counting refused calls keeps a sandbox that
        // ignores the refusal from probing forever at zero cost accounting.
        if self.spent.fetch_add(1, Ordering::SeqCst) >= self.budget {
            return Response::Error(ErrorResponse::new(
                ErrorCode::Denied,
                "the run's model-inference budget is exhausted",
                false,
            ));
        }
        let provider = tokio::select! {
            biased;
            () = self.cancel.cancelled() => return Self::cancelled_response(),
            provider = self.resolver.resolve() => provider,
        };
        match self.durable_egress_authorized().await {
            Ok(true) => {}
            Ok(false) => return Self::cancelled_response(),
            Err(()) => {
                return Response::Error(ErrorResponse::new(
                    ErrorCode::Internal,
                    "host model execution authority could not be verified",
                    true,
                ));
            }
        }
        // The sandbox names no model, provider, or endpoint: it supplies only a
        // prompt, and the host's policy-resolved config decides where it goes.
        let request = ChatRequest {
            provider: self.config.provider.clone(),
            model: self.config.model.clone(),
            reasoning_model: self.config.reasoning_model,
            messages: vec![ChatMessage::text(Role::User, params.prompt)],
            max_tokens: self.config.max_tokens,
            temperature: self.config.temperature,
            reasoning_effort: self.config.reasoning_effort,
            // One prompt in, one answer read once: nothing will ever extend
            // this prefix, so writing a cache entry for it only pays the write
            // premium for a read that cannot happen.
            prompt_cache: tidebreak_core::PromptCacheMode::OneShot,
            ..Default::default()
        };
        let stream = tokio::select! {
            biased;
            () = self.cancel.cancelled() => return Self::cancelled_response(),
            stream = provider.stream(request) => stream,
        };
        let mut stream = match stream {
            Ok(stream) => stream,
            Err(error) => {
                // A transport-safe, retryable refusal: re-issuing the same
                // operation identity may succeed once the provider is reachable.
                tracing::error!("tidebreak: host model proxy could not start inference: {error}");
                return Response::Error(ErrorResponse::new(
                    ErrorCode::Internal,
                    "host model inference could not start",
                    true,
                ));
            }
        };
        let mut completion = String::new();
        let mut observation = None;
        while let Some(event) = stream.next().await {
            match event {
                ProviderEvent::TextDelta { text } => {
                    self.observed.mark(&mut observation);
                    completion.push_str(&text);
                }
                ProviderEvent::Usage(reported) => {
                    if self.observed.add_usage(&mut observation, reported).is_err() {
                        if let Some(id) = observation {
                            let _ = self.account_observation(id).await;
                        }
                        return Response::Error(ErrorResponse::new(
                            ErrorCode::Internal,
                            "host model usage exceeded the supported total",
                            false,
                        ));
                    }
                }
                ProviderEvent::Stop { .. } | ProviderEvent::Refusal { .. } => {
                    self.observed.mark(&mut observation);
                }
                ProviderEvent::Failed { error } => {
                    if let Some(id) = observation {
                        if self.account_observation(id).await.is_err() {
                            return Response::Error(ErrorResponse::new(
                                ErrorCode::Internal,
                                "host model usage could not be recorded",
                                true,
                            ));
                        }
                    }
                    tracing::error!(
                        "tidebreak: host model proxy inference failed: {}",
                        error.message
                    );
                    return Response::Error(ErrorResponse::new(
                        ErrorCode::Internal,
                        "host model inference failed",
                        true,
                    ));
                }
                // These do not contribute to the completion text the sandbox
                // loop reads back, but they prove the provider started a model
                // step and therefore make cancellation accounting mandatory.
                ProviderEvent::ReasoningDelta { .. }
                | ProviderEvent::ReasoningBlock { .. }
                | ProviderEvent::ToolCallStarted { .. }
                | ProviderEvent::ToolCallArgsDelta { .. }
                | ProviderEvent::ProviderExecutedToolCall { .. } => {
                    self.observed.mark(&mut observation);
                }
                _ => {}
            }
        }
        if observation.is_none() {
            // A provider that reports neither usage nor a stop still completed
            // the stream Tidebreak is about to return. Count the step with zero
            // usage rather than inventing cache or token telemetry.
            self.observed.mark(&mut observation);
        }
        if self
            .account_observation(observation.expect("completed stream was observed"))
            .await
            .is_err()
        {
            return Response::Error(ErrorResponse::new(
                ErrorCode::Internal,
                "host model usage could not be recorded",
                true,
            ));
        }
        Response::Ok(ReverseResult::ModelInference(ModelInferenceResult {
            completion,
        }))
    }
}

/// Drives one admitted `container`-located agent run through a local container.
pub struct SandboxContainerRunner {
    store: Arc<dyn Store>,
    backend: Arc<dyn SandboxBackend>,
    resolver: Arc<dyn ProviderResolver>,
    config: SandboxContainerRunConfig,
    /// The issuer of run-scoped model tokens for detached-admitted runs, and
    /// the truthful source of the admission gate's
    /// `scoped_model_token_available` input. Defaults to the gateway position
    /// — unavailable, fail-closed — until the gateway can mint for real.
    token_issuer: Arc<dyn ScopedModelTokenIssuer>,
    /// Where this driver registers each exact claimed drive for cancellation
    /// and publishes the steering sink of every connection it holds. A driver
    /// assembled without the server's shared guard uses its own local guard.
    steering: Arc<SandboxSteerGuard>,
    /// Attached-drive heartbeat ticks observed by tests. Not a clock: the
    /// production loop still waits on `tokio::time::interval`. Tests wait on
    /// this count instead of assuming a cadence elapsed.
    #[cfg(test)]
    heartbeat_ticks: AtomicUsize,
}

impl SandboxContainerRunner {
    /// A runner over `store`, provisioning through `backend` and proxying model
    /// inference with `resolver`.
    #[must_use]
    pub fn new(
        store: Arc<dyn Store>,
        backend: Arc<dyn SandboxBackend>,
        resolver: Arc<dyn ProviderResolver>,
        config: SandboxContainerRunConfig,
    ) -> Self {
        Self {
            store,
            backend,
            resolver,
            config,
            token_issuer: Arc::new(GatewayScopedTokenIssuer),
            steering: Arc::new(SandboxSteerGuard::default()),
            #[cfg(test)]
            heartbeat_ticks: AtomicUsize::new(0),
        }
    }

    /// How many attached-drive heartbeat ticks this runner has issued.
    #[cfg(test)]
    pub(crate) fn heartbeat_ticks(&self) -> usize {
        self.heartbeat_ticks.load(Ordering::SeqCst)
    }

    /// Publish this driver's exact-drive cancellation handle and live-connection
    /// steering sinks into `steering`, the same guard the server routes use.
    #[must_use]
    pub(crate) fn with_steering(mut self, steering: Arc<SandboxSteerGuard>) -> Self {
        self.steering = steering;
        self
    }

    /// Replace the scoped-token issuer — the seam tests and future
    /// mint-capable deployments plug into. The default is fail-closed.
    // Production assembly keeps the default until a real issuer exists; the
    // seam is exercised by the tests meanwhile.
    #[cfg_attr(not(test), allow(dead_code))]
    #[must_use]
    pub(crate) fn with_token_issuer(mut self, issuer: Arc<dyn ScopedModelTokenIssuer>) -> Self {
        self.token_issuer = issuer;
        self
    }

    /// The detached-admission preconditions this process can establish for a
    /// local container run, each derived from the component that owns the fact
    /// — never a constant. The same shared shape backs the settings surface,
    /// so what settings names as missing is what this gate denies for.
    fn detached_preconditions(&self) -> DetachedPreconditions {
        crate::sandbox_admission::structural_preconditions(
            // The real fact from the configured issuer: true only when a
            // run-scoped, short-lived, revocable token can actually be minted.
            self.token_issuer.available(),
            self.backend.enforces_external_lifetime_cap(),
            self.backend.verifies_image_integrity(),
        )
    }

    /// Claim and drive the container-located run `run_id` to a terminal state.
    ///
    /// Returns `Ok(None)` if the run could not be claimed (it is not a queued
    /// container run, or another driver holds it). Otherwise it provisions,
    /// attaches, drives, commits, and — always, on any terminal path — drives the
    /// container's teardown obligation to completion before returning.
    ///
    /// # Errors
    /// Propagates a durable-store failure. A provisioning or transport failure is
    /// recorded as a terminal run failure rather than surfaced, because a
    /// sandbox-resident run has exactly one attempt and is never re-executed.
    pub async fn drive(&self, run_id: AgentRunId) -> Result<Option<SandboxContainerRunOutcome>> {
        let lease_token = Uuid::new_v4();
        let Some(run) = self
            .store
            .claim_container_agent_run(
                run_id,
                lease_token,
                chrono_duration(self.config.lease)?,
                self.config.max_concurrent_containers,
            )
            .await?
        else {
            return Ok(None);
        };
        if run.status != AgentRunStatus::Running
            || run.lease_token != Some(lease_token)
            || run.execution_location != AgentRunExecutionLocation::Container
        {
            return Err(AgentError::msg(format!(
                "claimed container agent run {run_id} has an invalid execution identity"
            )));
        }
        Ok(Some(self.drive_claimed(run, lease_token).await?))
    }

    /// Renew the exact container lease and resolve a no-op update against the
    /// authoritative execution fence.
    ///
    /// `heartbeat_agent_run` reports whether its conditional UPDATE changed a
    /// row, not whether the existing exact lease is still live. A renewal can
    /// therefore return `false` when the requested expiry is equal to the
    /// current expiry (for example in the same SQLite clock millisecond) or is
    /// already clamped to the run deadline. Only the validation read can
    /// distinguish that harmless no-op from cancellation or lease loss.
    async fn renew_or_validate_execution(
        store: Arc<dyn Store>,
        run_id: AgentRunId,
        lease_token: Uuid,
        lease_duration: chrono::Duration,
    ) -> Result<bool> {
        if store
            .heartbeat_agent_run(run_id, lease_token, lease_duration)
            .await?
        {
            return Ok(true);
        }
        store
            .validate_agent_run_execution(run_id, lease_token, AgentRunExecutionLocation::Container)
            .await
    }

    /// Race one setup operation against the exact local signal, the absolute
    /// deadline, and a short durable heartbeat cadence. The durable heartbeat
    /// is both lease maintenance and the cross-process cancellation watcher:
    /// `false` means cancellation or another terminal transition won under the
    /// shared claim lock, so the local token is tripped before this returns.
    async fn await_pre_attach<T, F>(
        &self,
        run: &AgentRun,
        lease_token: Uuid,
        cancel: &CancelToken,
        future: F,
    ) -> PreAttachEnd<T>
    where
        F: Future<Output = T>,
    {
        let future = future;
        tokio::pin!(future);
        let deadline = self.deadline_sleep(run);
        tokio::pin!(deadline);
        let fence_interval = self
            .config
            .durable_fence_interval
            .max(Duration::from_millis(1));
        let mut fence = tokio::time::interval(fence_interval);
        fence.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // The drive already performed an initial exact validation. Consume the
        // interval's immediate tick so a quick setup operation does not issue a
        // redundant write, while any blocked operation is still observed on the
        // short configured cadence.
        fence.tick().await;

        // At most one durable fence may exist at a time. It stays in this
        // select loop instead of being awaited inline: a setup transaction may
        // already hold the shared agent-run claim lock and need another poll to
        // release it, while the heartbeat is waiting for that same lock. Keep a
        // completed setup value until the in-flight fence settles so returning
        // cannot leave an unobserved lease check racing the next setup boundary.
        let mut completed = None;
        let mut durable_fence: Option<Pin<Box<dyn Future<Output = Result<bool>> + Send>>> = None;

        loop {
            if durable_fence.is_none() {
                if let Some(value) = completed.take() {
                    return PreAttachEnd::Completed(value);
                }
            }

            let setup_pending = completed.is_none();
            let fence_idle = durable_fence.is_none();
            let poll_durable_fence = async {
                match durable_fence.as_mut() {
                    Some(future) => future.as_mut().await,
                    None => std::future::pending::<Result<bool>>().await,
                }
            };
            let event = tokio::select! {
                biased;
                () = cancel.cancelled() => PreAttachPoll::Cancelled,
                result = poll_durable_fence => PreAttachPoll::FenceCompleted(result),
                () = &mut deadline => PreAttachPoll::DeadlineExceeded,
                value = &mut future, if setup_pending => PreAttachPoll::SetupCompleted(value),
                _ = fence.tick(), if fence_idle => PreAttachPoll::FenceTick,
            };
            match event {
                PreAttachPoll::Cancelled => return PreAttachEnd::Cancelled,
                PreAttachPoll::DeadlineExceeded => return PreAttachEnd::DeadlineExceeded,
                PreAttachPoll::SetupCompleted(value) => completed = Some(value),
                PreAttachPoll::FenceTick => {
                    let lease_duration = match chrono_duration(self.config.lease) {
                        Ok(duration) => duration,
                        Err(error) => return PreAttachEnd::FenceFailed(error),
                    };
                    let store = Arc::clone(&self.store);
                    let run_id = run.id;
                    durable_fence = Some(Box::pin(async move {
                        Self::renew_or_validate_execution(
                            store,
                            run_id,
                            lease_token,
                            lease_duration,
                        )
                        .await
                    }));
                }
                PreAttachPoll::FenceCompleted(Ok(true)) => durable_fence = None,
                PreAttachPoll::FenceCompleted(Ok(false)) => {
                    cancel.cancel();
                    return PreAttachEnd::Cancelled;
                }
                PreAttachPoll::FenceCompleted(Err(error)) => {
                    return PreAttachEnd::FenceFailed(error);
                }
            }
        }
    }

    /// A setup store error may be the losing side of an ambiguous or
    /// concurrently committed cancellation. Re-read durable state before
    /// returning it so this exact driver never abandons the only cancellation
    /// finalizer merely because model resolution or provisioning persistence
    /// failed at the same time.
    async fn setup_error_or_cancellation(
        &self,
        run: &AgentRun,
        lease_token: Uuid,
        cancel: &CancelToken,
        mut error: AgentError,
    ) -> Result<SandboxContainerRunOutcome> {
        let mut delay = TERMINAL_RETRY_INITIAL;
        loop {
            if cancel.is_cancelled() {
                return self
                    .finish_cancellation_with_retry(run.id, lease_token, run.deadline_at, delay)
                    .await;
            }
            match self.terminal_retry_state(run.id, lease_token).await {
                Ok(TerminalRetryState::OwnedCancelling) | Ok(TerminalRetryState::Cancelled) => {
                    cancel.cancel();
                    return self
                        .finish_cancellation_with_retry(run.id, lease_token, run.deadline_at, delay)
                        .await;
                }
                Ok(TerminalRetryState::OwnedRunning(_)) => return Err(error),
                Ok(
                    TerminalRetryState::Completed
                    | TerminalRetryState::Failed
                    | TerminalRetryState::Lost,
                ) => return Ok(SandboxContainerRunOutcome::LeaseLost(run.id)),
                Err(state_error) => error = state_error,
            }
            if !Self::wait_for_terminal_retry(run.deadline_at, &mut delay).await {
                return Err(error);
            }
        }
    }

    /// Terminalize a deterministic setup refusal while preserving a racing
    /// cancellation's immutable outcome and retrying ambiguous store errors.
    async fn fail_setup_with_retry(
        &self,
        run: &AgentRun,
        lease_token: Uuid,
        cancel: &CancelToken,
        code: &str,
        detail: &str,
    ) -> Result<SandboxContainerRunOutcome> {
        let mut delay = TERMINAL_RETRY_INITIAL;
        loop {
            if cancel.is_cancelled() {
                return self
                    .finish_cancellation_with_retry(run.id, lease_token, run.deadline_at, delay)
                    .await;
            }
            match self.fail(run.id, lease_token, code, detail).await {
                Ok(SandboxContainerRunOutcome::LeaseLost(_)) => {
                    return self
                        .finish_cancellation_with_retry(run.id, lease_token, run.deadline_at, delay)
                        .await;
                }
                Ok(outcome) => return Ok(outcome),
                Err(mut error) => {
                    match self.terminal_retry_state(run.id, lease_token).await {
                        Ok(TerminalRetryState::OwnedCancelling)
                        | Ok(TerminalRetryState::Cancelled) => {
                            cancel.cancel();
                            return self
                                .finish_cancellation_with_retry(
                                    run.id,
                                    lease_token,
                                    run.deadline_at,
                                    delay,
                                )
                                .await;
                        }
                        Ok(TerminalRetryState::Failed) => {
                            return Ok(SandboxContainerRunOutcome::Failed(run.id));
                        }
                        Ok(TerminalRetryState::Completed | TerminalRetryState::Lost) => {
                            return Ok(SandboxContainerRunOutcome::LeaseLost(run.id));
                        }
                        Ok(TerminalRetryState::OwnedRunning(lease_expires_at)) => {
                            if let Err(heartbeat_error) = self
                                .renew_terminal_lease_if_needed(
                                    run.id,
                                    lease_token,
                                    lease_expires_at,
                                )
                                .await
                            {
                                error = heartbeat_error;
                            }
                        }
                        Err(state_error) => error = state_error,
                    }
                    if !Self::wait_for_terminal_retry(run.deadline_at, &mut delay).await {
                        return Err(error);
                    }
                }
            }
        }
    }

    async fn drive_claimed(
        &self,
        run: AgentRun,
        lease_token: Uuid,
    ) -> Result<SandboxContainerRunOutcome> {
        let run_id = run.id;
        let Some(active_drive) = self.steering.register_container_drive(run_id, lease_token) else {
            return Ok(SandboxContainerRunOutcome::LeaseLost(run_id));
        };
        let cancel = active_drive.cancel_token();
        // Close cancel-before-register exactly as the in-process worker does:
        // once the local handle exists, revalidate the durable lease before any
        // policy resolution, provisioning, or provider work can begin.
        match self
            .store
            .validate_agent_run_execution(run_id, lease_token, AgentRunExecutionLocation::Container)
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                cancel.cancel();
                return self
                    .finish_cancellation_with_retry(
                        run_id,
                        lease_token,
                        run.deadline_at,
                        TERMINAL_RETRY_INITIAL,
                    )
                    .await;
            }
            Err(error) => {
                return self
                    .setup_error_or_cancellation(&run, lease_token, &cancel, error)
                    .await;
            }
        }
        // The run's durable identity is the protocol run identity: the operation
        // log, event cursor, and grant provenance are all scoped to it and
        // outlive any single connection to the container.
        let protocol_run_id = match RunId::from_uuid(*run_id.as_uuid()) {
            Ok(id) => id,
            Err(error) => {
                return self
                    .fail_setup_with_retry(
                        &run,
                        lease_token,
                        &cancel,
                        "invalid_run_identity",
                        &error.to_string(),
                    )
                    .await;
            }
        };
        let task = match run.input.clone() {
            Some(task) => task,
            None => {
                return self
                    .fail_setup_with_retry(
                        &run,
                        lease_token,
                        &cancel,
                        "missing_task",
                        "container agent run has no delegated task",
                    )
                    .await;
            }
        };
        // Resolve the run's model through the host's registry BEFORE any
        // container exists, and fail closed if it does not resolve: the in-process
        // worker gates every egress on the registry, and a container run must
        // egress under the same policy rather than a looser one. The chat's
        // network policy is compiled here too — the sandbox's egress proxy
        // enforces exactly the authority the owning conversation granted.
        let (config, network_policy) = match self
            .await_pre_attach(&run, lease_token, &cancel, self.resolve_model_config(&run))
            .await
        {
            PreAttachEnd::Completed(Ok(resolved)) => resolved,
            PreAttachEnd::Completed(Err(error)) => {
                return self
                    .fail_setup_with_retry(
                        &run,
                        lease_token,
                        &cancel,
                        "model_policy_refused",
                        &error.to_string(),
                    )
                    .await;
            }
            PreAttachEnd::Cancelled => {
                return self
                    .finish_cancellation_with_retry(
                        run_id,
                        lease_token,
                        run.deadline_at,
                        TERMINAL_RETRY_INITIAL,
                    )
                    .await;
            }
            PreAttachEnd::DeadlineExceeded => {
                return self
                    .commit_drive_end_with_retry(
                        run_id,
                        lease_token,
                        run.deadline_at,
                        &DriveEnd::DeadlineExceeded,
                    )
                    .await;
            }
            PreAttachEnd::FenceFailed(error) => {
                return self
                    .setup_error_or_cancellation(&run, lease_token, &cancel, error)
                    .await;
            }
        };
        if cancel.is_cancelled() {
            return self
                .finish_cancellation_with_retry(
                    run_id,
                    lease_token,
                    run.deadline_at,
                    TERMINAL_RETRY_INITIAL,
                )
                .await;
        }

        // The detached-admission gate (issue #824), evaluated before any
        // durable state is written and recorded on the provisioning intent:
        // every precondition from docs/sandbox-providers.md must hold or the
        // run is admitted attached-only. Every input is derived from the
        // component that owns the fact — the token issuer, the backend's
        // lifetime-cap declaration, the (still absent, #1188) image
        // verification — so for a local container today the decision is
        // structurally a denial, and it opens only when the real facts change.
        let admission_decision = evaluate_detached_admission(self.detached_preconditions());
        let mut admission = admission_decision.mode();

        // Durable provisioning intent — carrying the host-minted correlation
        // tag, its window, and the admission decision — committed before the
        // create call, so a crash on either side of the create converges
        // through the window lapse and the tag sweep instead of leaking a
        // container or double-provisioning.
        let run_uuid = *run_id.as_uuid();
        let tag = SandboxTag::new();
        let window_expires_at = chrono::Utc::now() + chrono_duration(self.config.provision_window)?;
        let tag_text = tag.to_string();
        let begin = self.store.begin_sandbox_provision_for_agent_run(
            run_id,
            lease_token,
            &tag_text,
            window_expires_at,
            admission,
        );
        let begin = match self
            .await_pre_attach(&run, lease_token, &cancel, begin)
            .await
        {
            PreAttachEnd::Completed(Ok(Some(outcome))) => outcome,
            PreAttachEnd::Completed(Ok(None)) | PreAttachEnd::Cancelled => {
                cancel.cancel();
                return self
                    .finish_cancellation_with_retry(
                        run_id,
                        lease_token,
                        run.deadline_at,
                        TERMINAL_RETRY_INITIAL,
                    )
                    .await;
            }
            PreAttachEnd::Completed(Err(error)) | PreAttachEnd::FenceFailed(error) => {
                return self
                    .setup_error_or_cancellation(&run, lease_token, &cancel, error)
                    .await;
            }
            PreAttachEnd::DeadlineExceeded => {
                return self
                    .commit_drive_end_with_retry(
                        run_id,
                        lease_token,
                        run.deadline_at,
                        &DriveEnd::DeadlineExceeded,
                    )
                    .await;
            }
        };
        let handle = match begin {
            BeginSandboxProvisionOutcome::Started => {
                // The intent transaction is the durable serialization point,
                // then this second exact read closes cancellation committed in
                // the return-to-caller gap before the external create is first
                // polled. A cancellation after this read may still race the
                // external call; the tag and post-create fence make that side
                // effect teardown-only and forbid attachment/provider egress.
                match self
                    .await_pre_attach(
                        &run,
                        lease_token,
                        &cancel,
                        self.store.validate_agent_run_execution(
                            run_id,
                            lease_token,
                            AgentRunExecutionLocation::Container,
                        ),
                    )
                    .await
                {
                    PreAttachEnd::Completed(Ok(true)) => {}
                    PreAttachEnd::Completed(Ok(false)) | PreAttachEnd::Cancelled => {
                        cancel.cancel();
                        return self
                            .finish_cancellation_with_retry(
                                run_id,
                                lease_token,
                                run.deadline_at,
                                TERMINAL_RETRY_INITIAL,
                            )
                            .await;
                    }
                    PreAttachEnd::Completed(Err(error)) | PreAttachEnd::FenceFailed(error) => {
                        let _ = self.store.enqueue_sandbox_teardown(run_uuid).await;
                        return self
                            .setup_error_or_cancellation(&run, lease_token, &cancel, error)
                            .await;
                    }
                    PreAttachEnd::DeadlineExceeded => {
                        return self
                            .commit_drive_end_with_retry(
                                run_id,
                                lease_token,
                                run.deadline_at,
                                &DriveEnd::DeadlineExceeded,
                            )
                            .await;
                    }
                }
                let provision = self.backend.provision(ProvisionRequest {
                    run_id: protocol_run_id,
                    tag,
                    // A local container has no external lifetime cap, which
                    // is why the run is attached-only.
                    lifetime_cap_secs: None,
                    network_policy,
                });
                let provisioned = self
                    .await_pre_attach(&run, lease_token, &cancel, provision)
                    .await;
                match provisioned {
                    PreAttachEnd::Cancelled => {
                        // The cancellation transition moves this handle-less
                        // intent to teardown. If dropping `provision` cannot
                        // stop an already-issued backend create, the tag is no
                        // longer live and the orphan sweep reclaims the
                        // ambiguous side effect without ever provisioning a
                        // second sandbox for this run.
                        return self
                            .finish_cancellation_with_retry(
                                run_id,
                                lease_token,
                                run.deadline_at,
                                TERMINAL_RETRY_INITIAL,
                            )
                            .await;
                    }
                    PreAttachEnd::DeadlineExceeded => {
                        // Use the same exact terminal reconciliation as an
                        // attached drive: a durable cancellation that raced the
                        // deadline keeps priority, while either terminal write
                        // turns the open intent into a teardown obligation.
                        return self
                            .commit_drive_end_with_retry(
                                run_id,
                                lease_token,
                                run.deadline_at,
                                &DriveEnd::DeadlineExceeded,
                            )
                            .await;
                    }
                    PreAttachEnd::FenceFailed(error) => {
                        // The backend future was dropped without proving
                        // whether its tagged side effect escaped cancellation.
                        // Disown the intent before surfacing the store failure;
                        // recovery can now reclaim only by the durable tag and
                        // no later driver can attach this create.
                        let _ = self.store.enqueue_sandbox_teardown(run_uuid).await;
                        return self
                            .setup_error_or_cancellation(&run, lease_token, &cancel, error)
                            .await;
                    }
                    PreAttachEnd::Completed(Ok(handle)) => {
                        // The backend call is the unavoidable external
                        // linearization gap. If cancellation committed after
                        // the exact intent fence, the tagged create may still
                        // have happened; from here it is teardown-only until a
                        // fresh durable heartbeat proves this exact claim is
                        // still running.
                        let commit = self
                            .store
                            .commit_sandbox_provision_handle(run_uuid, &handle.reference);
                        let committed = match self
                            .await_pre_attach(&run, lease_token, &cancel, commit)
                            .await
                        {
                            PreAttachEnd::Completed(Ok(committed)) => committed,
                            PreAttachEnd::Completed(Err(error))
                            | PreAttachEnd::FenceFailed(error) => {
                                self.teardown(run_uuid, &handle).await;
                                return self
                                    .setup_error_or_cancellation(&run, lease_token, &cancel, error)
                                    .await;
                            }
                            PreAttachEnd::Cancelled => {
                                self.teardown(run_uuid, &handle).await;
                                return self
                                    .finish_cancellation_with_retry(
                                        run_id,
                                        lease_token,
                                        run.deadline_at,
                                        TERMINAL_RETRY_INITIAL,
                                    )
                                    .await;
                            }
                            PreAttachEnd::DeadlineExceeded => {
                                self.teardown(run_uuid, &handle).await;
                                return self
                                    .commit_drive_end_with_retry(
                                        run_id,
                                        lease_token,
                                        run.deadline_at,
                                        &DriveEnd::DeadlineExceeded,
                                    )
                                    .await;
                            }
                        };
                        if !committed {
                            // The window lapsed mid-create and the sweep
                            // disowned the intent: this driver holds a
                            // container the durable state has already written
                            // off. Destroy it and fail on the intent.
                            self.teardown(run_uuid, &handle).await;
                            return self
                                .fail_setup_with_retry(
                                    &run,
                                    lease_token,
                                    &cancel,
                                    "provision_window_lapsed",
                                    "the provisioning window lapsed before the handle committed",
                                )
                                .await;
                        }
                        handle
                    }
                    PreAttachEnd::Completed(Err(error)) => {
                        // Whatever the create half-made carries this run's tag;
                        // the teardown obligation hands it to the tag sweep.
                        let _ = self.store.enqueue_sandbox_teardown(run_uuid).await;
                        return self
                            .fail_setup_with_retry(
                                &run,
                                lease_token,
                                &cancel,
                                "provision_failed",
                                &error.to_string(),
                            )
                            .await;
                    }
                }
            }
            BeginSandboxProvisionOutcome::Existing(record) => {
                // The durable record's admission decision wins over anything
                // this process would decide: no admission decision is
                // revisited by a disconnect or a crash, and recovery can
                // never upgrade a run to detached.
                admission = record.admission;
                match (record.state, record.handle) {
                    // A prior attempt of this same claim committed a handle and
                    // then lost its own commit: reconcile the container that
                    // already exists rather than creating a second one.
                    (SandboxProvisionState::Committed, Some(reference)) => {
                        let tag = record.tag.parse::<SandboxTag>().map_err(|_| {
                            AgentError::Store(format!(
                                "sandbox provision record for run {run_id} has a malformed tag"
                            ))
                        })?;
                        SandboxHandle { reference, tag }
                    }
                    // Intended (the prior attempt died before the handle
                    // committed) or already in teardown: this driver holds the
                    // claim, so no create can still be in flight — disown the
                    // intent and let the tag sweep reclaim whatever exists.
                    _ => {
                        let _ = self.store.enqueue_sandbox_teardown(run_uuid).await;
                        return self
                            .fail_setup_with_retry(
                                &run,
                                lease_token,
                                &cancel,
                                "provision_reclaimed",
                                "the run's provisioning record was not reconcilable",
                            )
                            .await;
                    }
                }
            }
        };

        // A remotely committed cancellation may have followed the exact
        // pre-create fence while the external backend call was in flight. The
        // create side effect is unavoidable in that ordering, but it is never
        // attached or allowed to egress: revalidate the exact durable claim
        // after the handle is persisted and tear it down on every losing path.
        match self
            .await_pre_attach(
                &run,
                lease_token,
                &cancel,
                self.store.validate_agent_run_execution(
                    run_id,
                    lease_token,
                    AgentRunExecutionLocation::Container,
                ),
            )
            .await
        {
            PreAttachEnd::Completed(Ok(true)) => {}
            PreAttachEnd::Completed(Ok(false)) | PreAttachEnd::Cancelled => {
                cancel.cancel();
                self.teardown(run_uuid, &handle).await;
                return self
                    .finish_cancellation_with_retry(
                        run_id,
                        lease_token,
                        run.deadline_at,
                        TERMINAL_RETRY_INITIAL,
                    )
                    .await;
            }
            PreAttachEnd::Completed(Err(error)) | PreAttachEnd::FenceFailed(error) => {
                self.teardown(run_uuid, &handle).await;
                return self
                    .setup_error_or_cancellation(&run, lease_token, &cancel, error)
                    .await;
            }
            PreAttachEnd::DeadlineExceeded => {
                self.teardown(run_uuid, &handle).await;
                return self
                    .commit_drive_end_with_retry(
                        run_id,
                        lease_token,
                        run.deadline_at,
                        &DriveEnd::DeadlineExceeded,
                    )
                    .await;
            }
        }

        // From here on a container exists, so every terminal path must drive its
        // teardown obligation.
        let outcome = self
            .attach_and_drive(
                &run,
                lease_token,
                protocol_run_id,
                config,
                task,
                &handle,
                admission,
                &cancel,
            )
            .await;
        self.teardown(run_uuid, &handle).await;
        outcome
    }

    /// Resolve the run's frozen model selection into an egress config under the
    /// host's model-registry policy, exactly as the in-process worker does, and
    /// compile the owning chat's network policy into the closed form the
    /// sandbox backend enforces.
    ///
    /// Fails closed: an absent or unregistered model refuses the run rather than
    /// egressing on an empty or unvetted selection.
    async fn resolve_model_config(
        &self,
        run: &AgentRun,
    ) -> Result<(AgentConfig, SandboxNetworkPolicy)> {
        let Some(model) = run.model.clone().filter(|model| !model.is_empty()) else {
            return Err(AgentError::config(
                "container agent run has no frozen model selection",
            ));
        };
        let chat = self
            .store
            .get_chat(run.chat_id)
            .await?
            .ok_or_else(|| AgentError::msg("container agent run has no chat"))?;
        let mut config = AgentConfig::default();
        if self.resolver.enforces_model_registry() {
            // Container sandboxes resolve without a caller snapshot: on a
            // hosted machine their model proxy has no per-caller route yet
            // either. Decision 62 names this gap and leaves it open.
            let Some(policy) =
                crate::providers::resolve_model_policy(&*self.store, &model, true, None).await?
            else {
                return Err(AgentError::config(
                    "container sandbox model is not registered for its provider",
                ));
            };
            if !crate::providers::is_valid_execution_policy(&policy) {
                return Err(AgentError::config(
                    "managed gateway execution requires a frozen model identity",
                ));
            }
            crate::providers::apply_model_policy(&mut config, &policy, chat.reasoning_effort)?;
        } else {
            // A test or custom embedder that injects one provider keeps its
            // free-form model contract, as elsewhere in the server — but a
            // registered model still runs under its own policy.
            crate::providers::apply_free_form_model(&mut config, model, chat.reasoning_effort)?;
        }
        let network_policy = crate::sandbox_docker::compile_network_policy(&chat.network_policy);
        Ok((config, network_policy))
    }

    #[allow(clippy::too_many_arguments)]
    async fn attach_and_drive(
        &self,
        run: &AgentRun,
        lease_token: Uuid,
        protocol_run_id: RunId,
        config: AgentConfig,
        task: String,
        handle: &SandboxHandle,
        admission: SandboxAdmissionMode,
        cancel: &CancelToken,
    ) -> Result<SandboxContainerRunOutcome> {
        let run_id = run.id;
        // One capability host per run, shared across every connection: it holds
        // the grant set, the model proxy, and the crash-safe operation log, and a
        // reattachment reuses it so a re-issued reverse call replays its recorded
        // answer rather than spending twice.
        let provenance = RunProvenance {
            run_id: protocol_run_id,
            provider: CONTAINER_PROVENANCE_PROVIDER.to_owned(),
        };
        // The operation log claims every fresh reverse inference identity
        // before this responder can reach the provider. Its cardinality is
        // therefore the durable spend-reservation count, including retryable
        // zero-observation failures and ambiguous calls left by a dead host.
        // Seed the process-local fast path from that crash-safe count rather
        // than completed `model_steps`, which intentionally excludes failed
        // attempts.
        let durable_spent = self.store.operation_log_len(run_id.0).await?;
        let model_proxy = Arc::new(HostModelProxy {
            resolver: Arc::clone(&self.resolver),
            cancel: cancel.clone(),
            lease_guard: Some(HostModelLeaseGuard {
                store: Arc::clone(&self.store),
                run_id,
                lease_token,
            }),
            config,
            spent: AtomicU32::new(u32::try_from(durable_spent).unwrap_or(u32::MAX)),
            budget: self.config.max_inference_operations,
            accounting: Some(HostModelAccounting {
                store: Arc::clone(&self.store),
                run_id,
                lease_token,
                baseline: tokio::sync::Mutex::new((run.model_steps, run.usage)),
            }),
            observed: HostModelObservedAccounting::default(),
        });
        let host = CapabilityHost::new(
            GrantSet::new(provenance, [Capability::ModelInference]),
            model_proxy.clone(),
            Arc::new(DurableOperationStore::new(
                Arc::clone(&self.store),
                protocol_run_id,
            )),
        );

        // The run init the host delivers after each attach: the task and the
        // policy snapshot, only ever over the authenticated connection — the
        // task no longer rides the container's environment, so a sandbox
        // reclaimed before its handle committed never executed anything.
        let deadline_unix_secs = run
            .deadline_at
            .map(|deadline| deadline.timestamp().max(0).unsigned_abs())
            .unwrap_or_default();
        // A detached-admitted run receives exactly one model credential: a
        // token minted for this run, expiring no later than its deadline. An
        // attached-only run carries none — the host is its model proxy. A
        // detached admission whose token cannot be minted (or whose issuer
        // overruns the cap) fails the run closed rather than delivering a
        // detached init without one; the run is never silently downgraded
        // against its durable admission record.
        let scoped_token = match self
            .await_pre_attach(
                run,
                lease_token,
                cancel,
                self.scoped_token_for(*run_id.as_uuid(), admission, deadline_unix_secs),
            )
            .await
        {
            PreAttachEnd::Completed(Ok(token)) => token,
            PreAttachEnd::Completed(Err(error)) => {
                return self
                    .fail_setup_with_retry(
                        run,
                        lease_token,
                        cancel,
                        "scoped_token_unavailable",
                        &error.to_string(),
                    )
                    .await;
            }
            PreAttachEnd::Cancelled => {
                return self
                    .finish_cancellation_with_retry(
                        run_id,
                        lease_token,
                        run.deadline_at,
                        TERMINAL_RETRY_INITIAL,
                    )
                    .await;
            }
            PreAttachEnd::DeadlineExceeded => {
                return self
                    .commit_drive_end_with_retry(
                        run_id,
                        lease_token,
                        run.deadline_at,
                        &DriveEnd::DeadlineExceeded,
                    )
                    .await;
            }
            PreAttachEnd::FenceFailed(error) => {
                return self
                    .setup_error_or_cancellation(run, lease_token, cancel, error)
                    .await;
            }
        };
        match self
            .await_pre_attach(
                run,
                lease_token,
                cancel,
                self.store.validate_agent_run_execution(
                    run_id,
                    lease_token,
                    AgentRunExecutionLocation::Container,
                ),
            )
            .await
        {
            PreAttachEnd::Completed(Ok(true)) => {}
            PreAttachEnd::Completed(Ok(false)) | PreAttachEnd::Cancelled => {
                cancel.cancel();
                return self
                    .finish_cancellation_with_retry(
                        run_id,
                        lease_token,
                        run.deadline_at,
                        TERMINAL_RETRY_INITIAL,
                    )
                    .await;
            }
            PreAttachEnd::Completed(Err(error)) | PreAttachEnd::FenceFailed(error) => {
                return self
                    .setup_error_or_cancellation(run, lease_token, cancel, error)
                    .await;
            }
            PreAttachEnd::DeadlineExceeded => {
                return self
                    .commit_drive_end_with_retry(
                        run_id,
                        lease_token,
                        run.deadline_at,
                        &DriveEnd::DeadlineExceeded,
                    )
                    .await;
            }
        }
        let init = RunInit {
            run_id: protocol_run_id,
            provenance: RunProvenance {
                run_id: protocol_run_id,
                provider: CONTAINER_PROVENANCE_PROVIDER.to_owned(),
            },
            task,
            deadline_unix_secs,
            // Derived from the durable admission decision — never a constant:
            // absent an admitting record, this is attached-only.
            admission: match admission {
                SandboxAdmissionMode::AttachedOnly => AdmissionMode::AttachedOnly,
                SandboxAdmissionMode::Detached => AdmissionMode::Detached,
            },
            policy: PolicySnapshot {
                egress_allowlist: Vec::new(),
                granted_capabilities: vec![Capability::ModelInference],
            },
            scoped_token,
        };

        // Drive the container while holding the lease live. A container run
        // routinely outlives one lease period, and the in-process reaper
        // terminalizes a background run whose lease expires — so without this
        // heartbeat the run is failed out from under a container that is still
        // working and still spending. The whole drive is additionally bounded by
        // the run's absolute deadline, so no path can wait forever.
        // The steering channel outlives any single connection: an instruction
        // accepted just as a connection drops is carried to the reattach rather
        // than lost. It is *not* a durable queue — it dies with this drive, and
        // nothing accepts steering while the run is unattached, because the
        // guard entry exists only while a connection is live.
        let (steer_tx, mut steer_rx) = tokio::sync::mpsc::channel::<String>(STEER_BACKLOG);
        let end = {
            // Keep the whole connection-owning drive in this scope. Whichever
            // terminal signal wins drops the drive (and therefore its active
            // `HostConnection`) before the capability host is closed and
            // quiesced below. The reader task can still be racing its abort, so
            // `CapabilityHost::close` remains the synchronized admission fence.
            let drive = self.drive_events(
                run_id,
                protocol_run_id,
                handle,
                &host,
                &init,
                lease_token,
                &steer_tx,
                &mut steer_rx,
            );
            tokio::pin!(drive);
            let heartbeat_interval = self
                .config
                .heartbeat
                .min(self.config.durable_fence_interval)
                .max(Duration::from_millis(1));
            let mut heartbeat = tokio::time::interval(heartbeat_interval);
            heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            // The first interval tick is immediate. Consume it so attach does
            // not issue a redundant write, then keep at most one durable
            // renewal in flight. A heartbeat that is awaited inline can sit on
            // the shared claim lock while model-step accounting needs another
            // poll of that same lock, and the connection drain never runs.
            heartbeat.tick().await;
            let deadline = self.deadline_sleep(run);
            tokio::pin!(deadline);
            let mut completed = None;
            let mut durable_heartbeat: Option<Pin<Box<dyn Future<Output = Result<bool>> + Send>>> =
                None;

            loop {
                if durable_heartbeat.is_none() {
                    if let Some(end) = completed.take() {
                        break end;
                    }
                }

                let drive_pending = completed.is_none();
                let heartbeat_idle = durable_heartbeat.is_none();
                let poll_durable_heartbeat = async {
                    match durable_heartbeat.as_mut() {
                        Some(future) => future.as_mut().await,
                        None => std::future::pending::<Result<bool>>().await,
                    }
                };
                let event = tokio::select! {
                    biased;
                    () = cancel.cancelled() => DrivePoll::Cancelled,
                    result = poll_durable_heartbeat => DrivePoll::HeartbeatCompleted(result),
                    () = &mut deadline => DrivePoll::DeadlineExceeded,
                    end = &mut drive, if drive_pending => DrivePoll::DriveCompleted(end),
                    _ = heartbeat.tick(), if heartbeat_idle => DrivePoll::HeartbeatTick,
                };
                match event {
                    DrivePoll::Cancelled => break DriveEnd::LeaseLost,
                    DrivePoll::DeadlineExceeded => break DriveEnd::DeadlineExceeded,
                    DrivePoll::DriveCompleted(end) => completed = Some(end),
                    DrivePoll::HeartbeatTick => {
                        let lease_duration = chrono_duration(self.config.lease)?;
                        let store = Arc::clone(&self.store);
                        durable_heartbeat = Some(Box::pin(async move {
                            Self::renew_or_validate_execution(
                                store,
                                run_id,
                                lease_token,
                                lease_duration,
                            )
                            .await
                        }));
                    }
                    DrivePoll::HeartbeatCompleted(Ok(true)) => {
                        durable_heartbeat = None;
                        #[cfg(test)]
                        self.heartbeat_ticks.fetch_add(1, Ordering::SeqCst);
                    }
                    DrivePoll::HeartbeatCompleted(Ok(false)) => {
                        // The lease is gone (cancelled, or reaped): stop driving
                        // rather than keep a container working for a run this
                        // host no longer owns. Teardown still runs.
                        break DriveEnd::LeaseLost;
                    }
                    DrivePoll::HeartbeatCompleted(Err(error)) => return Err(error),
                }
            }
        };

        // Every terminal transition uses the same ordering. Closing admission
        // and cancelling are one synchronized host operation: a racing reverse
        // dispatch is either already in the in-flight set and cancelled, or it
        // observes the closed state and never executes. Only after all responder
        // futures have dropped do we persist their observed usage/model steps;
        // the terminal CAS then snapshots those durable totals.
        host.close();
        host.wait_idle().await;
        self.finish_after_quiescence(run, lease_token, &model_proxy, &end)
            .await
    }

    /// Persist every provider step observed before quiescence and apply the
    /// terminal transition selected by the drive. Both writes are exact CASes,
    /// so retries recover an ambiguous commit rather than duplicating it.
    ///
    /// A durable cancellation can race any selected end variant. A fenced
    /// result/failure therefore never means cancellation has been acknowledged:
    /// after preserving any late result evidence, reconcile the exact owned
    /// `cancelling` state and commit its immutable cancellation receipt.
    async fn finish_after_quiescence(
        &self,
        run: &AgentRun,
        lease_token: Uuid,
        model_proxy: &HostModelProxy,
        end: &DriveEnd,
    ) -> Result<SandboxContainerRunOutcome> {
        self.flush_observed_accounting_with_retry(
            run.id,
            lease_token,
            run.deadline_at,
            model_proxy,
        )
        .await?;
        self.commit_drive_end_with_retry(run.id, lease_token, run.deadline_at, end)
            .await
    }

    /// Retry the final observed-step drain while this lease still owns a live
    /// running/cancelling run. `HostModelProxy` retains each observation until
    /// its exact baseline-and-delta CAS succeeds, and that CAS recovers an
    /// already-committed increment after an ambiguous response.
    async fn flush_observed_accounting_with_retry(
        &self,
        run_id: AgentRunId,
        lease_token: Uuid,
        deadline_at: Option<chrono::DateTime<chrono::Utc>>,
        model_proxy: &HostModelProxy,
    ) -> Result<()> {
        let mut delay = TERMINAL_RETRY_INITIAL;
        loop {
            let Err(mut error) = model_proxy.flush_observed_accounting().await else {
                return Ok(());
            };

            match self.terminal_retry_state(run_id, lease_token).await {
                Ok(TerminalRetryState::OwnedRunning(lease_expires_at)) => {
                    if let Err(heartbeat_error) = self
                        .renew_terminal_lease_if_needed(run_id, lease_token, lease_expires_at)
                        .await
                    {
                        error = heartbeat_error;
                    }
                }
                Ok(TerminalRetryState::OwnedCancelling) => {
                    match self
                        .renew_cancellation_finalization(run_id, lease_token)
                        .await
                    {
                        Ok(true) => {}
                        // The cancellation deadline or another terminal writer
                        // won while state was being inspected. Its immutable
                        // result is now authoritative, so no pending observation
                        // can still be appended under this identity.
                        Ok(false) => return Ok(()),
                        Err(renewal_error) => error = renewal_error,
                    }
                }
                // Another terminal writer or lease owner won. This driver can
                // no longer add accounting; continue to the terminal CAS so an
                // exact ambiguous result can recover and a late result can be
                // retained as evidence.
                Ok(
                    TerminalRetryState::Completed
                    | TerminalRetryState::Failed
                    | TerminalRetryState::Cancelled
                    | TerminalRetryState::Lost,
                ) => return Ok(()),
                Err(state_error) => error = state_error,
            }

            if !Self::wait_for_terminal_retry(deadline_at, &mut delay).await {
                return Err(error);
            }
        }
    }

    /// Apply the selected terminal transition with exact retry recovery. A
    /// fenced transition always gets one cancellation reconciliation before it
    /// is classified as lease loss, regardless of which `DriveEnd` won the
    /// in-memory select.
    async fn commit_drive_end_with_retry(
        &self,
        run_id: AgentRunId,
        lease_token: Uuid,
        deadline_at: Option<chrono::DateTime<chrono::Utc>>,
        end: &DriveEnd,
    ) -> Result<SandboxContainerRunOutcome> {
        let mut delay = TERMINAL_RETRY_INITIAL;
        loop {
            match self.commit_drive_end(run_id, lease_token, end).await {
                Ok(SandboxContainerRunOutcome::LeaseLost(_)) => {
                    return self
                        .finish_cancellation_with_retry(
                            run_id,
                            lease_token,
                            deadline_at,
                            TERMINAL_RETRY_INITIAL,
                        )
                        .await;
                }
                Ok(outcome) => return Ok(outcome),
                Err(mut error) => {
                    match self.terminal_retry_state(run_id, lease_token).await {
                        Ok(TerminalRetryState::OwnedRunning(lease_expires_at)) => {
                            if let Err(heartbeat_error) = self
                                .renew_terminal_lease_if_needed(
                                    run_id,
                                    lease_token,
                                    lease_expires_at,
                                )
                                .await
                            {
                                error = heartbeat_error;
                            }
                        }
                        Ok(TerminalRetryState::OwnedCancelling)
                        | Ok(TerminalRetryState::Cancelled) => {
                            return self
                                .finish_cancellation_with_retry(
                                    run_id,
                                    lease_token,
                                    deadline_at,
                                    delay,
                                )
                                .await;
                        }
                        // An error may have hidden a successful terminal CAS.
                        // Retry the same exact identity so the durable method
                        // can return its Existing receipt.
                        Ok(TerminalRetryState::Completed) if matches!(end, DriveEnd::Result(_)) => {
                        }
                        Ok(TerminalRetryState::Failed)
                            if matches!(
                                end,
                                DriveEnd::AgentFailed(_)
                                    | DriveEnd::TransportFailed(_)
                                    | DriveEnd::Unreachable
                                    | DriveEnd::DeadlineExceeded
                            ) => {}
                        Ok(
                            TerminalRetryState::Completed
                            | TerminalRetryState::Failed
                            | TerminalRetryState::Lost,
                        ) => return Ok(SandboxContainerRunOutcome::LeaseLost(run_id)),
                        Err(state_error) => error = state_error,
                    }

                    if !Self::wait_for_terminal_retry(deadline_at, &mut delay).await {
                        return Err(error);
                    }
                }
            }
        }
    }

    async fn commit_drive_end(
        &self,
        run_id: AgentRunId,
        lease_token: Uuid,
        end: &DriveEnd,
    ) -> Result<SandboxContainerRunOutcome> {
        match end {
            DriveEnd::Result(text) => self.commit_result(run_id, lease_token, text).await,
            DriveEnd::AgentFailed(detail) => {
                self.fail(run_id, lease_token, "sandbox_agent_failed", detail)
                    .await
            }
            DriveEnd::TransportFailed(detail) => {
                self.fail(run_id, lease_token, "sandbox_transport_failed", detail)
                    .await
            }
            DriveEnd::Unreachable => {
                self.fail(
                    run_id,
                    lease_token,
                    "sandbox_unreachable",
                    "container did not deliver a terminal event before its reattach budget",
                )
                .await
            }
            DriveEnd::DeadlineExceeded => {
                self.fail(
                    run_id,
                    lease_token,
                    "deadline_exceeded",
                    "container run exceeded its absolute deadline",
                )
                .await
            }
            DriveEnd::LeaseLost => Ok(SandboxContainerRunOutcome::LeaseLost(run_id)),
        }
    }

    /// Commit or recover the exact cancellation receipt. `Err` can be an
    /// ambiguous post-commit response, so a now-`cancelled` row is retryable;
    /// `Ok(None)` on that row is definitive proof the receipt belongs to a
    /// different identity.
    async fn finish_cancellation_with_retry(
        &self,
        run_id: AgentRunId,
        lease_token: Uuid,
        deadline_at: Option<chrono::DateTime<chrono::Utc>>,
        mut delay: Duration,
    ) -> Result<SandboxContainerRunOutcome> {
        loop {
            match self
                .store
                .finish_agent_run_cancellation(run_id, lease_token)
                .await
            {
                Ok(Some(_)) => return Ok(SandboxContainerRunOutcome::Cancelled(run_id)),
                Ok(None) => match self.terminal_retry_state(run_id, lease_token).await {
                    Ok(TerminalRetryState::OwnedCancelling) => {
                        match self
                            .renew_cancellation_finalization(run_id, lease_token)
                            .await
                        {
                            Ok(true) => {}
                            Ok(false) => {
                                return Ok(SandboxContainerRunOutcome::LeaseLost(run_id));
                            }
                            Err(error) => {
                                if !Self::wait_for_terminal_retry(deadline_at, &mut delay).await {
                                    return Err(error);
                                }
                                continue;
                            }
                        }
                        if !Self::wait_for_terminal_retry(deadline_at, &mut delay).await {
                            return Ok(SandboxContainerRunOutcome::LeaseLost(run_id));
                        }
                    }
                    Ok(
                        TerminalRetryState::OwnedRunning(_)
                        | TerminalRetryState::Completed
                        | TerminalRetryState::Failed
                        | TerminalRetryState::Cancelled
                        | TerminalRetryState::Lost,
                    ) => return Ok(SandboxContainerRunOutcome::LeaseLost(run_id)),
                    Err(error) => {
                        if !Self::wait_for_terminal_retry(deadline_at, &mut delay).await {
                            return Err(error);
                        }
                    }
                },
                Err(mut error) => {
                    match self.terminal_retry_state(run_id, lease_token).await {
                        Ok(TerminalRetryState::OwnedCancelling) => {
                            match self
                                .renew_cancellation_finalization(run_id, lease_token)
                                .await
                            {
                                Ok(true) => {}
                                Ok(false) => {
                                    return Ok(SandboxContainerRunOutcome::LeaseLost(run_id));
                                }
                                Err(renewal_error) => error = renewal_error,
                            }
                        }
                        Ok(TerminalRetryState::Cancelled) => {}
                        Ok(
                            TerminalRetryState::OwnedRunning(_)
                            | TerminalRetryState::Completed
                            | TerminalRetryState::Failed
                            | TerminalRetryState::Lost,
                        ) => return Ok(SandboxContainerRunOutcome::LeaseLost(run_id)),
                        Err(state_error) => error = state_error,
                    }
                    if !Self::wait_for_terminal_retry(deadline_at, &mut delay).await {
                        return Err(error);
                    }
                }
            }
        }
    }

    async fn terminal_retry_state(
        &self,
        run_id: AgentRunId,
        lease_token: Uuid,
    ) -> Result<TerminalRetryState> {
        let Some(run) = self.store.get_agent_run(run_id).await? else {
            return Ok(TerminalRetryState::Lost);
        };
        let now = chrono::Utc::now();
        let exact_token = run.lease_token == Some(lease_token);
        let deadline_open = run.deadline_at.is_some_and(|deadline| deadline > now);
        let exact_live_lease =
            exact_token && deadline_open && run.lease_expires_at.is_some_and(|expiry| expiry > now);
        Ok(match run.status {
            AgentRunStatus::Running if exact_live_lease => TerminalRetryState::OwnedRunning(
                run.lease_expires_at
                    .expect("an exact live running lease has an expiry"),
            ),
            // A durable cancellation freezes this token and claim identity:
            // no scheduler may supersede it while `cancelling`. The dedicated
            // renewal CAS below validates the immutable cancellation receipt
            // before reopening an expired lease for accounting/finalization.
            AgentRunStatus::Cancelling if exact_token && deadline_open => {
                TerminalRetryState::OwnedCancelling
            }
            AgentRunStatus::Completed => TerminalRetryState::Completed,
            AgentRunStatus::Failed => TerminalRetryState::Failed,
            AgentRunStatus::Cancelled => TerminalRetryState::Cancelled,
            _ => TerminalRetryState::Lost,
        })
    }

    async fn renew_terminal_lease_if_needed(
        &self,
        run_id: AgentRunId,
        lease_token: Uuid,
        lease_expires_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<()> {
        let renew_by = chrono::Utc::now() + chrono_duration(self.config.heartbeat)?;
        if lease_expires_at <= renew_by {
            // A false result can mean cancellation won between the state read
            // and heartbeat. The next retry re-reads and reconciles that state;
            // only a store error needs to be surfaced as the retry cause.
            let _ = self
                .store
                .heartbeat_agent_run(run_id, lease_token, chrono_duration(self.config.lease)?)
                .await?;
        }
        Ok(())
    }

    async fn renew_cancellation_finalization(
        &self,
        run_id: AgentRunId,
        lease_token: Uuid,
    ) -> Result<bool> {
        // Finalization does not authorize more container work, so its renewal
        // window may safely be wider than an unusually short execution lease.
        // Keep it long enough that the bounded retry backoff cannot expire the
        // authority between a successful renewal and the next exact CAS.
        let finalization_lease = self.config.lease.max(TERMINAL_RETRY_MAX.saturating_mul(2));
        self.store
            .renew_agent_run_cancellation_finalization(
                run_id,
                lease_token,
                chrono_duration(finalization_lease)?,
            )
            .await
    }

    async fn wait_for_terminal_retry(
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
        *delay = delay.saturating_mul(2).min(TERMINAL_RETRY_MAX);
        true
    }

    /// The scoped model token a run's admission entitles it to: `None` for an
    /// attached-only run, a freshly minted run-scoped token for a detached
    /// one — verified against the run's absolute deadline before delivery.
    ///
    /// # Errors
    /// Fails closed for a detached run when the issuer cannot mint, when the
    /// run carries no absolute deadline to cap the token by, or when the
    /// minted token would outlive that deadline (in which case whatever was
    /// minted is revoked before refusing).
    async fn scoped_token_for(
        &self,
        run_uuid: Uuid,
        admission: SandboxAdmissionMode,
        deadline_unix_secs: u64,
    ) -> Result<Option<ScopedModelToken>> {
        if admission != SandboxAdmissionMode::Detached {
            return Ok(None);
        }
        if deadline_unix_secs == 0 {
            return Err(AgentError::config(
                "a detached run requires an absolute deadline to cap its scoped token",
            ));
        }
        let minted = self.token_issuer.mint(run_uuid, deadline_unix_secs).await?;
        if minted.expires_at_unix_secs > deadline_unix_secs {
            // The issuer's claim is verified, not trusted: a token that would
            // outlive the run must never enter the container.
            self.revoke_scoped_token(run_uuid).await;
            return Err(AgentError::config(
                "the issuer minted a scoped token outliving the run deadline",
            ));
        }
        Ok(Some(minted.token))
    }

    /// Revoke the run's scoped token, best-effort. Idempotent, and a safe
    /// no-op for runs that never minted one; the mint-time lifetime cap still
    /// bounds the credential when the issuer cannot be reached.
    async fn revoke_scoped_token(&self, run_uuid: Uuid) {
        if let Err(error) = self.token_issuer.revoke(run_uuid).await {
            tracing::error!(
                "tidebreak: could not revoke the scoped model token for run {run_uuid}: {error}"
            );
        }
    }

    /// A sleep that fires at the run's absolute deadline. A run with no deadline
    /// (which the schema forbids for a background run) never fires, and the
    /// reattach budget still bounds the drive.
    async fn deadline_sleep(&self, run: &AgentRun) {
        let Some(deadline_at) = run.deadline_at else {
            return std::future::pending().await;
        };
        let remaining = deadline_at.signed_duration_since(chrono::Utc::now());
        // A negative remaining duration means the deadline already passed, and
        // `to_std` refuses it — fire immediately in that case.
        if let Ok(remaining) = remaining.to_std() {
            tokio::time::sleep(remaining).await;
        }
    }

    /// Attach and drain the container's event stream until it reports a terminal
    /// event, reattaching across unplanned disconnects within the budget.
    #[allow(clippy::too_many_arguments)]
    async fn drive_events(
        &self,
        run_id: AgentRunId,
        protocol_run_id: RunId,
        handle: &SandboxHandle,
        host: &CapabilityHost,
        init: &RunInit,
        lease_token: Uuid,
        steer_tx: &tokio::sync::mpsc::Sender<String>,
        steer_rx: &mut tokio::sync::mpsc::Receiver<String>,
    ) -> DriveEnd {
        let mut cursor = EventCursor::START;
        let mut attempt = 0u32;
        loop {
            match self
                .drain_connection(
                    run_id,
                    protocol_run_id,
                    handle,
                    host,
                    init,
                    &mut cursor,
                    lease_token,
                    steer_tx,
                    steer_rx,
                )
                .await
            {
                DrainOutcome::Result(text) => return DriveEnd::Result(text),
                DrainOutcome::AgentFailed(detail) => return DriveEnd::AgentFailed(detail),
                DrainOutcome::Disconnected => {
                    // An attached-only run that lost its host takes no new model
                    // step; reattachment resumes the stream from the committed
                    // cursor. Bound the retries so a container that never comes
                    // back is failed rather than driven forever.
                    if attempt >= self.config.reattach_attempts {
                        return DriveEnd::Unreachable;
                    }
                    attempt += 1;
                    tokio::time::sleep(self.config.reattach_backoff).await;
                }
                DrainOutcome::Failed(detail) => return DriveEnd::TransportFailed(detail),
            }
        }
    }

    /// Dial the container, attach, and drain its event stream over one
    /// connection until it delivers a result or drops.
    #[allow(clippy::too_many_arguments)]
    async fn drain_connection(
        &self,
        run_id: AgentRunId,
        protocol_run_id: RunId,
        handle: &SandboxHandle,
        host: &CapabilityHost,
        init: &RunInit,
        cursor: &mut EventCursor,
        lease_token: Uuid,
        steer_tx: &tokio::sync::mpsc::Sender<String>,
        steer_rx: &mut tokio::sync::mpsc::Receiver<String>,
    ) -> DrainOutcome {
        let address = match self.backend.address(handle).await {
            Ok(address) => address,
            Err(BackendError::UnknownHandle) => {
                return DrainOutcome::Failed("container no longer exists".to_owned());
            }
            Err(error) => {
                tracing::warn!("tidebreak: container address unavailable, will reattach: {error}");
                return DrainOutcome::Disconnected;
            }
        };
        let stream = match self.dial(&address).await {
            Ok(stream) => stream,
            Err(_) => return DrainOutcome::Disconnected,
        };
        let attach = AttachRequest {
            protocol_version: PROTOCOL_VERSION,
            run_id: protocol_run_id,
            resume_from: *cursor,
            // Present the per-run secret the backend minted and injected into the
            // container; the supervisor verifies it before installing this
            // connection. Cloned from the resolved address rather than discarded.
            transport_secret: address.transport_secret.clone(),
        };
        let mut conn = match WireClient::connect(stream, attach, host.clone()).await {
            Ok(conn) => conn,
            Err(error) => {
                // A version or authentication refusal is terminal — retrying the
                // same secret against the same container cannot succeed; a
                // transport failure during attach is a disconnect the driver
                // retries.
                return match error {
                    ConnectError::VersionRefused(_) => {
                        DrainOutcome::Failed("container speaks an incompatible protocol".to_owned())
                    }
                    ConnectError::Unauthenticated(_) => DrainOutcome::Failed(
                        "container rejected the run's transport secret".to_owned(),
                    ),
                    _ => DrainOutcome::Disconnected,
                };
            }
        };
        conn.start_keepalives(SANDBOX_KEEPALIVE_INTERVAL);
        // Deliver the run init on every attach; the sandbox keeps the first
        // and ignores redeliveries, so a reattach is idempotent.
        conn.send_init(init.clone()).await;
        // Publish this connection's steering sink for exactly as long as the
        // connection lives, so the API can tell "attached, instruction taken"
        // from "not attached, nothing queued" by whether an entry exists. A
        // registration refused (an identity already published) means a
        // superseded attempt still holds it; drive on rather than displacing it.
        let _attached = self
            .steering
            .register(run_id, lease_token, steer_tx.clone());
        // Drain events, committing the cursor by acknowledging each, until a
        // terminal event arrives or the connection closes, while forwarding any
        // steering the host accepted onto the reserved control lane. Both
        // terminal events end the drive: the supervisor keeps serving after its
        // agent loop returns, so waiting only for a result would hang on an open
        // socket and leak the container.
        loop {
            tokio::select! {
                event = conn.next_event() => {
                    let Some(event) = event else { return DrainOutcome::Disconnected };
                    let payload = event.payload.clone();
                    *cursor = EventCursor::committed(event.sequence);
                    conn.acknowledge(*cursor).await;
                    match payload {
                        EventPayload::Result(text) => return DrainOutcome::Result(text),
                        EventPayload::Failed(detail) => return DrainOutcome::AgentFailed(detail),
                        // Progress is observation, published so a reader can watch the
                        // run without waiting for its result. The sandbox's own event
                        // sequence keys the append, so a reattach that redelivers a
                        // batch leaves one line rather than two, and a failure to
                        // publish is dropped rather than allowed to end the drive.
                        EventPayload::Progress(text) => {
                            if let Err(error) = self
                                .store
                                .append_agent_run_progress(
                                    run_id,
                                    &format!("event:{}", event.sequence.get()),
                                    &text,
                                )
                                .await
                            {
                                tracing::error!("tidebreak: could not publish progress for run {run_id}: {error}");
                            }
                        }
                        _ => {}
                    }
                }
                Some(instruction) = steer_rx.recv() => {
                    if let Err(error) = conn.send_steer(SteerMessage::new(instruction)).await {
                        // The connection went away under the instruction. The
                        // drive reattaches; the instruction is not re-sent,
                        // because guidance the run never saw is the caller's to
                        // repeat, not the host's to replay later out of context.
                        tracing::warn!("tidebreak: a steering instruction was not delivered: {error}");
                    }
                }
            }
        }
    }

    /// Connect a TCP stream to the container's loopback address.
    async fn dial(&self, address: &SandboxAddress) -> Result<TcpStream> {
        let authority = address
            .base_url
            .trim_start_matches("http://")
            .trim_start_matches("https://");
        let connect = TcpStream::connect(authority);
        match tokio::time::timeout(self.config.dial_timeout, connect).await {
            Ok(Ok(stream)) => Ok(stream),
            Ok(Err(error)) => Err(AgentError::msg(format!("container dial failed: {error}"))),
            Err(_) => Err(AgentError::msg("container dial timed out")),
        }
    }

    async fn commit_result(
        &self,
        run_id: AgentRunId,
        lease_token: Uuid,
        text: &str,
    ) -> Result<SandboxContainerRunOutcome> {
        match self
            .store
            .submit_agent_run_result(run_id, lease_token, text)
            .await?
        {
            Some(SubmitAgentRunResultOutcome::Completed(_)) => {
                Ok(SandboxContainerRunOutcome::Completed(run_id))
            }
            Some(SubmitAgentRunResultOutcome::Existing(_)) => {
                Ok(SandboxContainerRunOutcome::AlreadyCompleted(run_id))
            }
            None => {
                // The fenced commit refused: the run is already terminal or the
                // lease is gone. The container it truly ran in still produced a
                // well-formed result — retain it as non-authoritative evidence
                // on the provisioning record, never commit it.
                match self
                    .store
                    .record_late_container_result_evidence(*run_id.as_uuid(), text)
                    .await
                {
                    Ok(true) => tracing::info!(
                        "tidebreak: retained a late container result for run {run_id} as evidence"
                    ),
                    Ok(false) => {}
                    Err(error) => tracing::error!(
                        "tidebreak: could not retain a late container result for run {run_id}: {error}"
                    ),
                }
                Ok(SandboxContainerRunOutcome::LeaseLost(run_id))
            }
        }
    }

    async fn fail(
        &self,
        run_id: AgentRunId,
        lease_token: Uuid,
        code: &str,
        detail: &str,
    ) -> Result<SandboxContainerRunOutcome> {
        let detail = detail
            .chars()
            .take(AgentRun::MAX_ERROR_DETAIL_LEN)
            .collect::<String>();
        // A sandbox-resident run is admitted with a single attempt, so this
        // exhausts the attempt budget and terminalizes rather than scheduling a
        // retry. The delay is required to be positive but is never waited on
        // here — a container run is never re-claimed.
        match self
            .store
            .fail_agent_run(
                run_id,
                lease_token,
                code,
                &detail,
                chrono::Duration::seconds(1),
            )
            .await?
        {
            Some(_) => Ok(SandboxContainerRunOutcome::Failed(run_id)),
            None => Ok(SandboxContainerRunOutcome::LeaseLost(run_id)),
        }
    }

    /// Drive the container's teardown obligation, idempotently. The obligation
    /// is persisted before the first destroy attempt and marked done only on a
    /// confirmed destroy, so an unconfirmed teardown survives this process and
    /// is re-driven by [`sweep`](Self::sweep) rather than abandoned.
    async fn teardown(&self, run_uuid: Uuid, handle: &SandboxHandle) {
        // Every terminal path drives teardown, so this is where a detached
        // run's scoped token dies with the run — before the container is even
        // destroyed, and idempotently for runs that never minted one.
        self.revoke_scoped_token(run_uuid).await;
        if let Err(error) = self.store.enqueue_sandbox_teardown(run_uuid).await {
            tracing::error!(
                "tidebreak: could not persist a container teardown obligation: {error}"
            );
        }
        for attempt in 0..3u32 {
            match self.backend.destroy(handle).await {
                Ok(()) => {
                    if let Err(error) = self.store.complete_sandbox_teardown(run_uuid).await {
                        tracing::warn!(
                            "tidebreak: container teardown confirmed but not recorded: {error}"
                        );
                    }
                    return;
                }
                Err(error) => {
                    tracing::warn!(
                        "tidebreak: container teardown attempt {attempt} unconfirmed: {error}"
                    );
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
        tracing::warn!(
            "tidebreak: container teardown for {} left unconfirmed; the sweep re-drives it",
            handle.reference
        );
    }

    /// Recover container runs whose driver died: reclaim each expired-lease
    /// run under a fresh lease — the **same** execution attempt, since exactly
    /// one container was ever asked to run it — and drive it to a terminal
    /// state.
    ///
    /// The drive that follows reconciles through the durable records: a
    /// committed handle reattaches to the existing container and first drains
    /// whatever it buffered while unattached (a finished run's result commits
    /// instead of being reaped), the operation log replays recorded reverse
    /// answers rather than spending twice, and every terminal path drives the
    /// teardown obligation. A run whose deadline already crossed is not
    /// reclaimed here — the claim scan fails it, enqueueing its teardown in
    /// the same transition.
    ///
    /// # Errors
    /// Propagates a durable-store failure; per-run drive outcomes are returned,
    /// not raised.
    pub async fn recover(&self) -> Result<Vec<SandboxContainerRunOutcome>> {
        let mut outcomes = Vec::new();
        for run in self
            .store
            .list_reclaimable_container_agent_runs(chrono::Utc::now())
            .await?
        {
            let lease_token = Uuid::new_v4();
            let Some(claimed) = self
                .store
                .reclaim_container_agent_run(
                    run.id,
                    lease_token,
                    chrono_duration(self.config.lease)?,
                )
                .await?
            else {
                // Lost to a racing driver or a crossed deadline; both leave
                // the run in owned hands.
                continue;
            };
            outcomes.push(self.drive_claimed(claimed, lease_token).await?);
        }
        Ok(outcomes)
    }

    /// One recovery pass over the durable provisioning records: lapse every
    /// `Intended` record whose window expired, drive every pending teardown
    /// obligation, and reclaim any backend sandbox whose tag names no live
    /// record.
    ///
    /// Idempotent, and safe to run beside live drivers: the lapse is predicated
    /// on `intended` (so it can never disown a committed handle), the live-tag
    /// set is read *after* the lapse (so the tag sweep can never race a slow
    /// in-flight create — an unlapsed intent's tag stays live), and destroy is
    /// idempotent at the backend.
    ///
    /// # Errors
    /// Propagates a durable-store failure. Backend failures leave the
    /// obligations in place for the next pass.
    pub async fn sweep(&self) -> Result<()> {
        let lapsed = self
            .store
            .lapse_sandbox_provisions(chrono::Utc::now())
            .await?;
        for record in &lapsed {
            tracing::warn!(
                "tidebreak: sandbox provisioning intent for run {} lapsed; its tag is reclaimable",
                record.run_id
            );
            self.revoke_scoped_token(record.run_id).await;
        }

        // Directed destroys first: obligations with a committed handle name
        // their container exactly. Each obligation is a run some terminal
        // path (this driver's, the reaper's, or an unattached cancellation's)
        // wrote off, so its scoped token is revoked here too — the reaper
        // path's revocation, for runs whose own driver never got to it.
        for record in self.store.list_sandbox_teardowns().await? {
            self.revoke_scoped_token(record.run_id).await;
            let Some(reference) = record.handle.clone() else {
                continue;
            };
            let Ok(tag) = record.tag.parse::<SandboxTag>() else {
                continue;
            };
            let handle = SandboxHandle { reference, tag };
            if self.backend.destroy(&handle).await.is_ok() {
                self.store.complete_sandbox_teardown(record.run_id).await?;
            }
        }

        // The tag sweep: destroy everything the records disown. On `Ok` the
        // backend guarantees no sandbox outside the live set remains, which is
        // what lets a handle-less obligation (a lapsed intent whose create may
        // or may not have reached the provider) be marked done.
        // Freeze the obligations this sweep is entitled to discharge before
        // taking the live-tag snapshot. A teardown committed after that
        // snapshot may have been represented there as live and therefore
        // deliberately preserved by the backend; completing a fresh listing
        // would lose its only retry record without destroying its container.
        let sweep_teardowns = self
            .store
            .list_sandbox_teardowns()
            .await?
            .into_iter()
            .map(|record| record.run_id)
            .collect::<Vec<_>>();
        let live: std::collections::HashSet<SandboxTag> = self
            .store
            .live_sandbox_tags()
            .await?
            .iter()
            .filter_map(|tag| tag.parse().ok())
            .collect();
        match self.backend.reclaim_orphans(&live).await {
            Ok(reclaimed) => {
                for handle in &reclaimed {
                    tracing::info!(
                        "tidebreak: reclaimed an orphaned sandbox container {}",
                        handle.reference
                    );
                }
                for run_id in sweep_teardowns {
                    self.store.complete_sandbox_teardown(run_id).await?;
                }
            }
            Err(error) => {
                tracing::warn!(
                    "tidebreak: the sandbox orphan sweep proved nothing this pass: {error}"
                );
            }
        }
        Ok(())
    }
}

/// How one fallible pre-attach await ended. Cancellation and deadline are
/// explicit so the setup future is dropped before terminal storage work begins;
/// a durable-fence failure is retained separately so the caller can reconcile a
/// concurrently committed immutable cancellation before propagating it.
enum PreAttachEnd<T> {
    Completed(T),
    Cancelled,
    DeadlineExceeded,
    FenceFailed(AgentError),
}

/// One poll result from [`SandboxContainerRunner::await_pre_attach`]. Keeping
/// the setup and durable-fence completions distinct lets the runner retain one
/// while continuing to poll the other without ever spawning overlapping lease
/// heartbeats.
enum PreAttachPoll<T> {
    SetupCompleted(T),
    Cancelled,
    DeadlineExceeded,
    FenceTick,
    FenceCompleted(Result<bool>),
}

/// One poll result from the attached-drive heartbeat loop. The connection
/// drain and the durable lease renewal stay distinct so a heartbeat write
/// cannot starve the in-flight reverse-RPC that still needs to be polled.
enum DrivePoll<T> {
    Cancelled,
    DeadlineExceeded,
    DriveCompleted(T),
    HeartbeatTick,
    HeartbeatCompleted(Result<bool>),
}

/// The result of draining one connection to the container.
enum DrainOutcome {
    /// The container delivered its terminal result.
    Result(String),
    /// The container's agent loop ended without a result — it exhausted its step
    /// budget or a model step failed. Terminal, and distinct from a transport
    /// failure: the container worked and reported an outcome.
    AgentFailed(String),
    /// The connection dropped (or could not be established) with no terminal
    /// event; an attached-only run reattaches and resumes from the committed
    /// cursor.
    Disconnected,
    /// A terminal transport condition (version refusal, vanished container).
    Failed(String),
}

/// How the whole drive ended, across every connection to the container.
enum DriveEnd {
    /// The container submitted a result to commit.
    Result(String),
    /// The container's agent loop ended without a result.
    AgentFailed(String),
    /// A terminal transport condition.
    TransportFailed(String),
    /// The container never came back within the reattach budget.
    Unreachable,
    /// The run's absolute deadline passed while the container worked.
    DeadlineExceeded,
    /// The lease was lost mid-drive; this host no longer owns the run.
    LeaseLost,
}

/// Durable state relevant to deciding whether a transient post-quiescence
/// write can be retried under this exact container lease.
enum TerminalRetryState {
    OwnedRunning(chrono::DateTime<chrono::Utc>),
    OwnedCancelling,
    Completed,
    Failed,
    Cancelled,
    Lost,
}

fn chrono_duration(duration: Duration) -> Result<chrono::Duration> {
    chrono::Duration::from_std(duration)
        .map_err(|error| AgentError::msg(format!("invalid container-run duration: {error}")))
}

#[cfg(test)]
mod tests;
