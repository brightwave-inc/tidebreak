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

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use openwave_core::{
    AgentConfig, AgentError, AgentRun, AgentRunExecutionLocation, AgentRunId, AgentRunStatus,
    ChatMessage, ChatRequest, ProviderEvent, Result, Role, Store, SubmitAgentRunResultOutcome,
};
use openwave_sandbox_protocol::{
    events::EventPayload,
    ids::{EventCursor, RunId},
    protocol::{ErrorCode, ErrorResponse, Response, PROTOCOL_VERSION},
    reverse::{
        Capability, CapabilityResponder, GrantSet, ModelInferenceResult, ReverseRequest,
        ReverseResult, RunProvenance,
    },
    AttachRequest, BackendError, CapabilityHost, ConnectError, ProvisionRequest, SandboxAddress,
    SandboxBackend, SandboxHandle, SandboxTag, WireClient,
};
use tokio::net::TcpStream;
use uuid::Uuid;

use crate::durable_oplog::DurableOperationStore;
use crate::resolver::ProviderResolver;

/// The provider attribution stamped on reverse operations from a local
/// container. Untrusted attribution rendered on consent prompts, never a claim
/// the container makes about itself.
const CONTAINER_PROVENANCE_PROVIDER: &str = "local-container";

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
    /// How long to wait for one TCP dial of the container's loopback address.
    pub dial_timeout: Duration,
    /// How many times the driver re-dials after an unplanned disconnect before
    /// giving up on an attached-only run. Reattachment resumes the event stream
    /// from the committed cursor and replays any recorded reverse answer.
    pub reattach_attempts: u32,
    /// Backoff between re-dials.
    pub reattach_backoff: Duration,
}

impl Default for SandboxContainerRunConfig {
    fn default() -> Self {
        Self {
            lease: Duration::from_secs(60),
            heartbeat: Duration::from_secs(15),
            dial_timeout: Duration::from_secs(10),
            reattach_attempts: 5,
            reattach_backoff: Duration::from_millis(250),
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
    /// The run's model selection already resolved through the host's model
    /// registry (provider route, reasoning shape, token and effort bounds), so
    /// every proxied completion egresses under exactly the policy an in-process
    /// run would. Resolved once at attach and failed closed there, never
    /// re-derived per request from an untrusted prompt.
    config: AgentConfig,
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
        let provider = self.resolver.resolve().await;
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
            ..Default::default()
        };
        let mut stream = match provider.stream(request).await {
            Ok(stream) => stream,
            Err(error) => {
                // A transport-safe, retryable refusal: re-issuing the same
                // operation identity may succeed once the provider is reachable.
                eprintln!("openwave: host model proxy could not start inference: {error}");
                return Response::Error(ErrorResponse::new(
                    ErrorCode::Internal,
                    "host model inference could not start",
                    true,
                ));
            }
        };
        let mut completion = String::new();
        while let Some(event) = stream.next().await {
            match event {
                ProviderEvent::TextDelta { text } => completion.push_str(&text),
                ProviderEvent::Failed { message } => {
                    eprintln!("openwave: host model proxy inference failed: {message}");
                    return Response::Error(ErrorResponse::new(
                        ErrorCode::Internal,
                        "host model inference failed",
                        true,
                    ));
                }
                // Reasoning, usage, tool-call, and stop events do not contribute
                // to the completion text the sandbox loop reads back.
                _ => {}
            }
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
        }
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
            .claim_container_agent_run(run_id, lease_token, chrono_duration(self.config.lease)?)
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

    async fn drive_claimed(
        &self,
        run: AgentRun,
        lease_token: Uuid,
    ) -> Result<SandboxContainerRunOutcome> {
        let run_id = run.id;
        // The run's durable identity is the protocol run identity: the operation
        // log, event cursor, and grant provenance are all scoped to it and
        // outlive any single connection to the container.
        let protocol_run_id = match RunId::from_uuid(*run_id.as_uuid()) {
            Ok(id) => id,
            Err(error) => {
                return self
                    .fail(
                        run_id,
                        lease_token,
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
                    .fail(
                        run_id,
                        lease_token,
                        "missing_task",
                        "container agent run has no delegated task",
                    )
                    .await;
            }
        };
        // Resolve the run's model through the host's registry BEFORE any
        // container exists, and fail closed if it does not resolve: the in-process
        // worker gates every egress on the registry, and a container run must
        // egress under the same policy rather than a looser one.
        let config = match self.resolve_model_config(&run).await {
            Ok(config) => config,
            Err(error) => {
                return self
                    .fail(
                        run_id,
                        lease_token,
                        "model_policy_refused",
                        &error.to_string(),
                    )
                    .await;
            }
        };

        // Provisioning intent and correlation tag before the create call: the
        // host mints the tag, and the backend stamps it into the container's
        // metadata so an orphan sweep can reclaim a container whose run never
        // committed a handle. (The durable intent row and the host-side sweep it
        // drives are a follow-up; the tag discipline and the backend's sweep are
        // here.)
        let tag = SandboxTag::new();
        let handle = match self
            .backend
            .provision(ProvisionRequest {
                run_id: protocol_run_id,
                tag,
                // A local container has no external lifetime cap, which is why
                // the run is attached-only.
                lifetime_cap_secs: None,
                // The delegated task. Interim delivery: see `ProvisionRequest::task`.
                task: Some(task),
            })
            .await
        {
            Ok(handle) => handle,
            Err(error) => {
                return self
                    .fail(run_id, lease_token, "provision_failed", &error.to_string())
                    .await;
            }
        };

        // From here on a container exists, so every terminal path must drive its
        // teardown obligation.
        let outcome = self
            .attach_and_drive(&run, lease_token, protocol_run_id, config, &handle)
            .await;
        self.teardown(&handle).await;
        outcome
    }

    /// Resolve the run's frozen model selection into an egress config under the
    /// host's model-registry policy, exactly as the in-process worker does.
    ///
    /// Fails closed: an absent or unregistered model refuses the run rather than
    /// egressing on an empty or unvetted selection.
    async fn resolve_model_config(&self, run: &AgentRun) -> Result<AgentConfig> {
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
            let Some(policy) =
                crate::providers::resolve_model_policy(&*self.store, &model, true).await?
            else {
                return Err(AgentError::config(
                    "container sandbox model is not registered for its provider",
                ));
            };
            crate::providers::apply_model_policy(&mut config, &policy, chat.reasoning_effort)?;
        } else {
            // A test or custom embedder that injects one provider keeps its
            // free-form model contract, as elsewhere in the server.
            config.model = model;
            config.reasoning_effort = chat.reasoning_effort;
        }
        Ok(config)
    }

    async fn attach_and_drive(
        &self,
        run: &AgentRun,
        lease_token: Uuid,
        protocol_run_id: RunId,
        config: AgentConfig,
        handle: &SandboxHandle,
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
        let host = CapabilityHost::new(
            GrantSet::new(provenance, [Capability::ModelInference]),
            Arc::new(HostModelProxy {
                resolver: Arc::clone(&self.resolver),
                config,
            }),
            Arc::new(DurableOperationStore::new(
                Arc::clone(&self.store),
                protocol_run_id,
            )),
        );

        // Drive the container while holding the lease live. A container run
        // routinely outlives one lease period, and the in-process reaper
        // terminalizes a background run whose lease expires — so without this
        // heartbeat the run is failed out from under a container that is still
        // working and still spending. The whole drive is additionally bounded by
        // the run's absolute deadline, so no path can wait forever.
        let drive = self.drive_events(protocol_run_id, handle, &host);
        tokio::pin!(drive);
        let mut heartbeat = tokio::time::interval(self.config.heartbeat);
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        heartbeat.tick().await;
        let deadline = self.deadline_sleep(run);
        tokio::pin!(deadline);

        let end = loop {
            tokio::select! {
                end = &mut drive => break end,
                () = &mut deadline => break DriveEnd::DeadlineExceeded,
                _ = heartbeat.tick() => {
                    if !self
                        .store
                        .heartbeat_agent_run(
                            run_id,
                            lease_token,
                            chrono_duration(self.config.lease)?,
                        )
                        .await?
                    {
                        // The lease is gone (cancelled, or reaped): stop driving
                        // rather than keep a container working for a run this
                        // host no longer owns. Teardown still runs.
                        break DriveEnd::LeaseLost;
                    }
                }
            }
        };

        match end {
            DriveEnd::Result(text) => self.commit_result(run_id, lease_token, &text).await,
            DriveEnd::AgentFailed(detail) => {
                self.fail(run_id, lease_token, "sandbox_agent_failed", &detail)
                    .await
            }
            DriveEnd::TransportFailed(detail) => {
                self.fail(run_id, lease_token, "sandbox_transport_failed", &detail)
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
    async fn drive_events(
        &self,
        protocol_run_id: RunId,
        handle: &SandboxHandle,
        host: &CapabilityHost,
    ) -> DriveEnd {
        let mut cursor = EventCursor::START;
        let mut attempt = 0u32;
        loop {
            match self
                .drain_connection(protocol_run_id, handle, host, &mut cursor)
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
    async fn drain_connection(
        &self,
        protocol_run_id: RunId,
        handle: &SandboxHandle,
        host: &CapabilityHost,
        cursor: &mut EventCursor,
    ) -> DrainOutcome {
        let address = match self.backend.address(handle).await {
            Ok(address) => address,
            Err(BackendError::UnknownHandle) => {
                return DrainOutcome::Failed("container no longer exists".to_owned());
            }
            Err(error) => {
                eprintln!("openwave: container address unavailable, will reattach: {error}");
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
        };
        let mut conn = match WireClient::connect(stream, attach, host.clone()).await {
            Ok(conn) => conn,
            Err(error) => {
                // A version refusal is terminal; a transport failure during
                // attach is a disconnect the driver retries.
                return match error {
                    ConnectError::VersionRefused(_) => {
                        DrainOutcome::Failed("container speaks an incompatible protocol".to_owned())
                    }
                    _ => DrainOutcome::Disconnected,
                };
            }
        };
        // Drain events, committing the cursor by acknowledging each, until a
        // terminal event arrives or the connection closes. Both terminal events
        // end the drive: the supervisor keeps serving after its agent loop
        // returns, so waiting only for a result would hang on an open socket and
        // leak the container.
        while let Some(event) = conn.next_event().await {
            let payload = event.payload.clone();
            *cursor = EventCursor::committed(event.sequence);
            conn.acknowledge(*cursor).await;
            match payload {
                EventPayload::Result(text) => return DrainOutcome::Result(text),
                EventPayload::Failed(detail) => return DrainOutcome::AgentFailed(detail),
                _ => {}
            }
        }
        DrainOutcome::Disconnected
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
            None => Ok(SandboxContainerRunOutcome::LeaseLost(run_id)),
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

    /// Drive the container's teardown obligation to completion, idempotently. A
    /// container that outlives its run is a bounded exposure the destroy exists
    /// to end, so an unconfirmed destroy is retried within a small budget rather
    /// than abandoned.
    async fn teardown(&self, handle: &SandboxHandle) {
        for attempt in 0..3u32 {
            match self.backend.destroy(handle).await {
                Ok(()) => return,
                Err(error) => {
                    eprintln!(
                        "openwave: container teardown attempt {attempt} unconfirmed: {error}"
                    );
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
        eprintln!(
            "openwave: container teardown for {} left unconfirmed; a sweep must re-drive it",
            handle.reference
        );
    }
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

fn chrono_duration(duration: Duration) -> Result<chrono::Duration> {
    chrono::Duration::from_std(duration)
        .map_err(|error| AgentError::msg(format!("invalid container-run duration: {error}")))
}

#[cfg(test)]
mod tests;
