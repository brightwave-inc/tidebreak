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
    AgentError, AgentRun, AgentRunExecutionLocation, AgentRunId, AgentRunStatus, ChatMessage,
    ChatRequest, ProviderEvent, Result, Role, Store, SubmitAgentRunResultOutcome,
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
    /// How long to wait for one TCP dial of the container's loopback address.
    pub dial_timeout: Duration,
    /// Upper bound on tokens the host requests per proxied model completion.
    pub max_tokens: u32,
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
            dial_timeout: Duration::from_secs(10),
            max_tokens: 4_096,
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
    model: String,
    max_tokens: u32,
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
        let request = ChatRequest {
            model: self.model.clone(),
            messages: vec![ChatMessage::text(Role::User, params.prompt)],
            max_tokens: Some(self.max_tokens),
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
        let model = run.model.clone().unwrap_or_default();

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
            .attach_and_drive(&run, lease_token, protocol_run_id, task, model, &handle)
            .await;
        self.teardown(&handle).await;
        outcome
    }

    #[allow(clippy::too_many_arguments)]
    async fn attach_and_drive(
        &self,
        run: &AgentRun,
        lease_token: Uuid,
        protocol_run_id: RunId,
        task: String,
        model: String,
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
                model,
                max_tokens: self.config.max_tokens,
            }),
            Arc::new(DurableOperationStore::new(
                Arc::clone(&self.store),
                protocol_run_id,
            )),
        );
        // The task is delivered out of band into the container (its environment
        // today); the run-init frame is a protocol follow-up. Retained here so
        // the driver is the single place that would deliver it once the frame
        // lands.
        let _ = task;

        let mut cursor = EventCursor::START;
        let mut attempt = 0u32;
        loop {
            match self
                .drain_connection(protocol_run_id, handle, &host, &mut cursor)
                .await
            {
                DrainOutcome::Result(text) => {
                    return self.commit_result(run_id, lease_token, &text).await;
                }
                DrainOutcome::Disconnected => {
                    // An attached-only run that lost its host takes no new model
                    // step; reattachment resumes the stream from the committed
                    // cursor. Bound the retries so a container that never comes
                    // back is failed rather than driven forever.
                    if attempt >= self.config.reattach_attempts {
                        return self
                            .fail(
                                run_id,
                                lease_token,
                                "sandbox_unreachable",
                                "container did not deliver a result before its reattach budget",
                            )
                            .await;
                    }
                    attempt += 1;
                    tokio::time::sleep(self.config.reattach_backoff).await;
                }
                DrainOutcome::Failed(detail) => {
                    return self
                        .fail(run_id, lease_token, "sandbox_transport_failed", &detail)
                        .await;
                }
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
        // Drain events, committing the cursor by acknowledging each, until the
        // result arrives or the connection closes.
        while let Some(event) = conn.next_event().await {
            let payload = event.payload.clone();
            *cursor = EventCursor::committed(event.sequence);
            conn.acknowledge(*cursor).await;
            if let EventPayload::Result(text) = payload {
                return DrainOutcome::Result(text);
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
    /// The connection dropped (or could not be established) with no result; an
    /// attached-only run reattaches and resumes from the committed cursor.
    Disconnected,
    /// A terminal transport condition (version refusal, vanished container).
    Failed(String),
}

fn chrono_duration(duration: Duration) -> Result<chrono::Duration> {
    chrono::Duration::from_std(duration)
        .map_err(|error| AgentError::msg(format!("invalid container-run duration: {error}")))
}

#[cfg(test)]
mod tests;
