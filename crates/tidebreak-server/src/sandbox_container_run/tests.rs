//! Tests for the sandbox-resident container driver.
//!
//! The non-Docker tests drive the **real** in-container agent loop
//! (`tidebreak_sandbox_agent::run_agent` plus its sandbox-resident tool registry
//! and the sandbox transport server) over a real loopback TCP socket, with only Docker
//! (the [`SandboxBackend`]) and the host model (the [`ProviderResolver`]) mocked.
//! That exercises the whole stack — provision, attach, reverse-RPC model
//! inference answered by the host proxy through the durable op-log, event drain,
//! fenced result commit, teardown — without a container runtime. The Docker
//! end-to-end test at the bottom re-points the same driver at a real container
//! and is skipped cleanly when no daemon is present.
//!
//! Every socket test is wrapped in a timeout so a regression fails loudly rather
//! than hanging CI, and every test uses the multi-thread runtime the durable
//! operation store's `block_in_place` bridge requires.

use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::{self, BoxStream};
use futures::StreamExt;
use tidebreak_core::{
    AdmitSandboxAgentRunOutcome, AgentConfig, AgentError, AgentRun, AgentRunExecutionLocation,
    AgentRunId, AgentRunStatus, CallId, CancelToken, Chat, ChatId, ChatRequest, DbStore,
    ModelProvider, ProviderEvent, ProviderId, Result, StopReason, Store,
};
use tidebreak_sandbox_agent::{run_agent, STEERING_PREFIX};
use tidebreak_sandbox_protocol::{
    ids::{OperationId, RunId, SandboxTag},
    protocol::{ErrorCode, Response, PROTOCOL_VERSION},
    reverse::{
        Capability, CapabilityResponder, GrantSet, ModelInferenceParams, ReverseEnvelope,
        ReverseRequest, ReverseResult, RunProvenance,
    },
    serve_connection, BackendError, CapabilityHost, ProvisionRequest, ReverseOutcome,
    SandboxAddress, SandboxBackend, SandboxHandle, SandboxRun, TransportSecret,
};
use tokio::net::TcpListener;
use tokio::sync::Notify;
use uuid::Uuid;

use super::{
    DriveEnd, HostModelAccounting, HostModelObservedAccounting, HostModelProxy, PreAttachEnd,
    SandboxContainerRunConfig, SandboxContainerRunOutcome, SandboxContainerRunner,
};
use crate::durable_oplog::DurableOperationStore;
use crate::resolver::ProviderResolver;
use crate::sandbox_admission::{
    evaluate_detached_admission, DetachedAdmission, DetachedAdmissionDenial,
};
use crate::sandbox_container_run_worker::{
    SandboxContainerRunWorker, SandboxContainerRunWorkerConfig,
};
use crate::scoped_model_token::{MintedScopedToken, ScopedModelTokenIssuer};
use crate::state::{SandboxSteerGuard, SandboxSteerRefusal};

// --- Mock host model (the resolver the driver proxies inference through) ------

/// A provider that scripts one directive step then a final answer, counting how
/// many completions it is asked for. Drives the real in-container loop: the first
/// completion tells the sandbox to run a filesystem tool, the second is the
/// final result the sandbox submits.
/// Holds the sandbox's first model step open so a test can act while the run is
/// genuinely attached and working: the provider announces the step on `started`
/// and answers only once the test signals `release`.
#[derive(Default)]
struct StepGate {
    started: Notify,
    release: Notify,
}

struct ScriptedProvider {
    completions: Mutex<Vec<String>>,
    calls: AtomicUsize,
    /// Every prompt the sandbox asked the host to complete. The sandbox's
    /// transcript opens with `Task: <task>`, so this is where a test reads back
    /// which task the container actually received.
    prompts: Mutex<Vec<String>>,
    /// How long each completion stalls, so a test can hold a drive open across
    /// several lease periods.
    delay: Duration,
    /// When set, the first completion is held at this gate.
    gate: Option<Arc<StepGate>>,
}

impl ScriptedProvider {
    fn new(completions: Vec<String>) -> Self {
        Self {
            completions: Mutex::new(completions),
            calls: AtomicUsize::new(0),
            prompts: Mutex::new(Vec::new()),
            delay: Duration::ZERO,
            gate: None,
        }
    }

    /// The same provider, but holding its first completion at `gate`.
    fn gated(completions: Vec<String>, gate: Arc<StepGate>) -> Self {
        Self {
            gate: Some(gate),
            ..Self::new(completions)
        }
    }

    /// The same provider, but stalling `delay` before answering each completion,
    /// so a drive spans several lease periods.
    fn slow(completions: Vec<String>, delay: Duration) -> Self {
        Self {
            delay,
            ..Self::new(completions)
        }
    }

    fn first_prompt(&self) -> String {
        self.prompts
            .lock()
            .unwrap()
            .first()
            .cloned()
            .unwrap_or_default()
    }
}

#[async_trait]
impl ModelProvider for ScriptedProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("scripted-host-model")
    }

    async fn stream(&self, request: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        let index = self.calls.fetch_add(1, Ordering::SeqCst);
        if index == 0 {
            if let Some(gate) = &self.gate {
                gate.started.notify_one();
                gate.release.notified().await;
            }
        }
        for message in &request.messages {
            for block in &message.content {
                if let tidebreak_core::ContentBlock::Text { text } = block {
                    self.prompts.lock().unwrap().push(text.clone());
                }
            }
        }
        let text = self
            .completions
            .lock()
            .unwrap()
            .get(index)
            .cloned()
            .unwrap_or_else(|| "the final answer".to_owned());
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        Ok(stream::iter(vec![
            ProviderEvent::TextDelta { text },
            ProviderEvent::Stop {
                reason: StopReason::EndTurn,
            },
        ])
        .boxed())
    }
}

/// A resolver that always hands back the same scripted provider, so completion
/// counts are observable and the op-log's exactly-once holds across a re-issue.
struct FixedResolver(Arc<ScriptedProvider>);

#[async_trait]
impl ProviderResolver for FixedResolver {
    async fn resolve(&self) -> Arc<dyn ModelProvider> {
        self.0.clone()
    }
}

struct UsageThenPendingProvider {
    stalled: Arc<Notify>,
    dropped: Arc<AtomicBool>,
    drop_observed: Arc<Notify>,
    drop_gate: Option<Arc<Barrier>>,
    calls: AtomicUsize,
    usage: tidebreak_core::Usage,
}

struct UsageThenPendingStream {
    stalled: Arc<Notify>,
    dropped: Arc<AtomicBool>,
    drop_observed: Arc<Notify>,
    drop_gate: Option<Arc<Barrier>>,
    usage: Option<tidebreak_core::Usage>,
    announced: bool,
}

impl futures::Stream for UsageThenPendingStream {
    type Item = ProviderEvent;

    fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if let Some(usage) = self.usage.take() {
            return Poll::Ready(Some(ProviderEvent::Usage(usage)));
        }
        if !self.announced {
            self.announced = true;
            self.stalled.notify_one();
        }
        Poll::Pending
    }
}

impl Drop for UsageThenPendingStream {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
        self.drop_observed.notify_one();
        if let Some(gate) = &self.drop_gate {
            gate.wait();
        }
    }
}

#[async_trait]
impl ModelProvider for UsageThenPendingProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("usage-then-pending")
    }

    async fn stream(&self, _request: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        let call_index = self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(UsageThenPendingStream {
            stalled: self.stalled.clone(),
            dropped: self.dropped.clone(),
            drop_observed: self.drop_observed.clone(),
            drop_gate: (call_index == 0).then(|| self.drop_gate.clone()).flatten(),
            usage: Some(self.usage),
            announced: false,
        }
        .boxed())
    }
}

struct UsageThenPendingResolver(Arc<UsageThenPendingProvider>);

#[async_trait]
impl ProviderResolver for UsageThenPendingResolver {
    async fn resolve(&self) -> Arc<dyn ModelProvider> {
        self.0.clone()
    }
}

/// Store seam for deterministic post-quiescence fault injection. The runner
/// only sees this wrapper during the final accounting/terminal phase; fixture
/// setup and assertions use the underlying real database directly.
struct TerminalFaultStore {
    inner: Arc<dyn Store>,
    setup_fault: Option<SetupFault>,
    setup_claim_lock: tokio::sync::Mutex<()>,
    setup_claim_lock_held: AtomicBool,
    setup_entered: Notify,
    setup_release: Notify,
    fence_entered: Notify,
    block_next_result: AtomicBool,
    result_entered: Notify,
    result_release: Notify,
    fail_accounting: AtomicBool,
    accounting_failure_observed: Notify,
    accounting_calls: AtomicUsize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SetupFault {
    ModelResolution,
    ProvisionIntentDelayed,
    ProvisionIntentFailure,
    ProvisionIntentClaimLockYield,
}

impl TerminalFaultStore {
    fn new(inner: Arc<dyn Store>) -> Arc<Self> {
        Self::with_optional_setup_fault(inner, None)
    }

    /// Shared-store seam for setup races. The durable cancellation is
    /// committed through the underlying database, while this process is held
    /// in one fallible pre-attach await and owns no process-local signal from
    /// the cancelling caller.
    fn with_setup_fault(inner: Arc<dyn Store>, fault: SetupFault) -> Arc<Self> {
        Self::with_optional_setup_fault(inner, Some(fault))
    }

    fn with_optional_setup_fault(
        inner: Arc<dyn Store>,
        setup_fault: Option<SetupFault>,
    ) -> Arc<Self> {
        Arc::new(Self {
            inner,
            setup_fault,
            setup_claim_lock: tokio::sync::Mutex::new(()),
            setup_claim_lock_held: AtomicBool::new(false),
            setup_entered: Notify::new(),
            setup_release: Notify::new(),
            fence_entered: Notify::new(),
            block_next_result: AtomicBool::new(false),
            result_entered: Notify::new(),
            result_release: Notify::new(),
            fail_accounting: AtomicBool::new(false),
            accounting_failure_observed: Notify::new(),
            accounting_calls: AtomicUsize::new(0),
        })
    }

    fn block_next_result(&self) {
        self.block_next_result.store(true, Ordering::SeqCst);
    }

    fn fail_accounting_until_released(&self) {
        self.fail_accounting.store(true, Ordering::SeqCst);
    }

    fn release_accounting(&self) {
        self.fail_accounting.store(false, Ordering::SeqCst);
    }
}

#[async_trait]
impl Store for TerminalFaultStore {
    async fn create_project(&self, project: &tidebreak_core::Project) -> Result<()> {
        self.inner.create_project(project).await
    }

    async fn get_project(
        &self,
        id: tidebreak_core::ProjectId,
    ) -> Result<Option<tidebreak_core::Project>> {
        self.inner.get_project(id).await
    }

    async fn list_projects(&self) -> Result<Vec<tidebreak_core::Project>> {
        self.inner.list_projects().await
    }

    async fn create_chat(&self, chat: &Chat) -> Result<()> {
        self.inner.create_chat(chat).await
    }

    async fn create_chat_with_project_defaults(&self, chat: &Chat) -> Result<Chat> {
        self.inner.create_chat_with_project_defaults(chat).await
    }

    async fn get_chat(&self, id: ChatId) -> Result<Option<Chat>> {
        if self.setup_fault == Some(SetupFault::ModelResolution) {
            self.setup_entered.notify_one();
            self.setup_release.notified().await;
            return Err(AgentError::Store(
                "injected model-resolution storage failure".into(),
            ));
        }
        self.inner.get_chat(id).await
    }

    async fn list_chats(&self) -> Result<Vec<Chat>> {
        self.inner.list_chats().await
    }

    async fn get_chat_transcript(
        &self,
        id: ChatId,
    ) -> Result<Option<tidebreak_core::ChatTranscriptSnapshot>> {
        self.inner.get_chat_transcript(id).await
    }

    async fn set_chat_model(&self, id: ChatId, model: Option<String>) -> Result<()> {
        self.inner.set_chat_model(id, model).await
    }

    async fn set_chat_title(&self, id: ChatId, title: Option<String>) -> Result<()> {
        self.inner.set_chat_title(id, title).await
    }

    async fn set_chat_title_if_unset(&self, id: ChatId, title: &str) -> Result<bool> {
        self.inner.set_chat_title_if_unset(id, title).await
    }

    async fn update_chat_metadata(
        &self,
        id: ChatId,
        title: Option<Option<String>>,
        model: Option<Option<String>>,
        reasoning_effort: Option<Option<tidebreak_core::ReasoningEffort>>,
        permission_mode: Option<Option<tidebreak_core::PermissionMode>>,
        network_policy: Option<tidebreak_core::NetworkPolicy>,
    ) -> Result<bool> {
        self.inner
            .update_chat_metadata(
                id,
                title,
                model,
                reasoning_effort,
                permission_mode,
                network_policy,
            )
            .await
    }

    async fn resumed_sandbox_spawn_batch(
        &self,
        turn_id: tidebreak_core::TurnId,
        attempt_count: i32,
        claim_count: i32,
    ) -> Result<Vec<tidebreak_core::SandboxAgentSpawnRequest>> {
        self.inner
            .resumed_sandbox_spawn_batch(turn_id, attempt_count, claim_count)
            .await
    }

    async fn get_agent_run(&self, id: AgentRunId) -> Result<Option<AgentRun>> {
        self.inner.get_agent_run(id).await
    }

    async fn claim_container_agent_run(
        &self,
        id: AgentRunId,
        lease_token: Uuid,
        lease_duration: chrono::Duration,
        max_running_containers: u32,
    ) -> Result<Option<AgentRun>> {
        self.inner
            .claim_container_agent_run(id, lease_token, lease_duration, max_running_containers)
            .await
    }

    async fn begin_sandbox_provision_for_agent_run(
        &self,
        run_id: AgentRunId,
        lease_token: Uuid,
        tag: &str,
        window_expires_at: chrono::DateTime<chrono::Utc>,
        admission: tidebreak_core::SandboxAdmissionMode,
    ) -> Result<Option<tidebreak_core::BeginSandboxProvisionOutcome>> {
        if self.setup_fault == Some(SetupFault::ProvisionIntentClaimLockYield) {
            // Model the real provisioning transaction after it has acquired
            // the agent-run claim lock but while an awaited database operation
            // still needs another poll to complete and release that lock.
            let claim_lock = self.setup_claim_lock.lock().await;
            self.setup_claim_lock_held.store(true, Ordering::SeqCst);
            self.setup_entered.notify_one();
            self.setup_release.notified().await;
            drop(claim_lock);
            self.setup_claim_lock_held.store(false, Ordering::SeqCst);
        }
        if matches!(
            self.setup_fault,
            Some(SetupFault::ProvisionIntentDelayed | SetupFault::ProvisionIntentFailure)
        ) {
            self.setup_entered.notify_one();
            self.setup_release.notified().await;
        }
        if self.setup_fault == Some(SetupFault::ProvisionIntentFailure) {
            return Err(AgentError::Store(
                "injected provisioning-intent storage failure".into(),
            ));
        }
        self.inner
            .begin_sandbox_provision_for_agent_run(
                run_id,
                lease_token,
                tag,
                window_expires_at,
                admission,
            )
            .await
    }

    async fn validate_agent_run_execution(
        &self,
        run_id: AgentRunId,
        lease_token: Uuid,
        execution_location: AgentRunExecutionLocation,
    ) -> Result<bool> {
        self.inner
            .validate_agent_run_execution(run_id, lease_token, execution_location)
            .await
    }

    async fn record_agent_run_model_step(
        &self,
        id: AgentRunId,
        lease_token: Uuid,
        expected_model_steps: i32,
        expected_usage: tidebreak_core::Usage,
        usage: tidebreak_core::Usage,
    ) -> Result<tidebreak_core::storage::RecordAgentRunModelStepOutcome> {
        self.accounting_calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_accounting.load(Ordering::SeqCst) {
            self.accounting_failure_observed.notify_one();
            return Err(AgentError::Store(
                "injected transient container accounting failure".into(),
            ));
        }
        self.inner
            .record_agent_run_model_step(
                id,
                lease_token,
                expected_model_steps,
                expected_usage,
                usage,
            )
            .await
    }

    async fn renew_agent_run_cancellation_finalization(
        &self,
        id: AgentRunId,
        lease_token: Uuid,
        lease_duration: chrono::Duration,
    ) -> Result<bool> {
        self.inner
            .renew_agent_run_cancellation_finalization(id, lease_token, lease_duration)
            .await
    }

    async fn heartbeat_agent_run(
        &self,
        id: AgentRunId,
        lease_token: Uuid,
        lease_duration: chrono::Duration,
    ) -> Result<bool> {
        if self.setup_fault == Some(SetupFault::ProvisionIntentClaimLockYield)
            && self.setup_claim_lock_held.load(Ordering::SeqCst)
        {
            // A periodic durable fence contends on the same serialization lock
            // as the held setup transaction. The notification makes the
            // deadlock interleaving deterministic for the regression below.
            self.fence_entered.notify_one();
            let _claim_lock = self.setup_claim_lock.lock().await;
        }
        self.inner
            .heartbeat_agent_run(id, lease_token, lease_duration)
            .await
    }

    async fn finish_agent_run_cancellation(
        &self,
        id: AgentRunId,
        lease_token: Uuid,
    ) -> Result<Option<tidebreak_core::storage::FinishAgentRunCancellationOutcome>> {
        self.inner
            .finish_agent_run_cancellation(id, lease_token)
            .await
    }

    async fn submit_agent_run_result(
        &self,
        id: AgentRunId,
        lease_token: Uuid,
        text: &str,
    ) -> Result<Option<tidebreak_core::storage::SubmitAgentRunResultOutcome>> {
        if self.block_next_result.swap(false, Ordering::SeqCst) {
            self.result_entered.notify_one();
            self.result_release.notified().await;
        }
        self.inner
            .submit_agent_run_result(id, lease_token, text)
            .await
    }

    async fn record_late_container_result_evidence(
        &self,
        run_id: Uuid,
        text: &str,
    ) -> Result<bool> {
        self.inner
            .record_late_container_result_evidence(run_id, text)
            .await
    }

    async fn append_message(&self, message: &tidebreak_core::Message) -> Result<()> {
        self.inner.append_message(message).await
    }

    async fn list_messages(&self, chat_id: ChatId) -> Result<Vec<tidebreak_core::Message>> {
        self.inner.list_messages(chat_id).await
    }

    async fn accept_tool_call(
        &self,
        call: &tidebreak_core::ToolCallRecord,
    ) -> Result<tidebreak_core::AcceptToolCallOutcome> {
        self.inner.accept_tool_call(call).await
    }

    async fn claim_client_tool_call(
        &self,
        id: CallId,
        chat_id: ChatId,
        executor_id: Uuid,
        lease_token: Uuid,
        now: chrono::DateTime<chrono::Utc>,
        lease_expires_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<tidebreak_core::ClaimClientToolCallOutcome> {
        self.inner
            .claim_client_tool_call(id, chat_id, executor_id, lease_token, now, lease_expires_at)
            .await
    }

    async fn heartbeat_client_tool_call(
        &self,
        id: CallId,
        chat_id: ChatId,
        lease_token: Uuid,
        now: chrono::DateTime<chrono::Utc>,
        lease_expires_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<tidebreak_core::HeartbeatClientToolCallOutcome> {
        self.inner
            .heartbeat_client_tool_call(id, chat_id, lease_token, now, lease_expires_at)
            .await
    }

    async fn resolve_server_tool_call(
        &self,
        id: CallId,
        resolution: &tidebreak_core::ToolCallResolution,
        resolved_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<tidebreak_core::ResolveToolCallOutcome> {
        self.inner
            .resolve_server_tool_call(id, resolution, resolved_at)
            .await
    }

    async fn resolve_client_tool_call_and_append_event(
        &self,
        id: CallId,
        chat_id: ChatId,
        lease_token: Uuid,
        now: chrono::DateTime<chrono::Utc>,
        resolution: &tidebreak_core::ToolCallResolution,
        resolved_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<tidebreak_core::JournaledClientToolCallOutcome> {
        self.inner
            .resolve_client_tool_call_and_append_event(
                id,
                chat_id,
                lease_token,
                now,
                resolution,
                resolved_at,
            )
            .await
    }

    async fn resolve_expired_client_tool_call_and_append_event(
        &self,
        id: CallId,
        chat_id: ChatId,
        lease_token: Uuid,
        now: chrono::DateTime<chrono::Utc>,
        resolution: &tidebreak_core::ToolCallResolution,
        resolved_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<tidebreak_core::JournaledClientToolCallOutcome> {
        self.inner
            .resolve_expired_client_tool_call_and_append_event(
                id,
                chat_id,
                lease_token,
                now,
                resolution,
                resolved_at,
            )
            .await
    }

    async fn list_pending_client_tool_calls(
        &self,
        chat_id: ChatId,
    ) -> Result<Vec<tidebreak_core::ToolCallRecord>> {
        self.inner.list_pending_client_tool_calls(chat_id).await
    }

    async fn list_tool_calls(
        &self,
        chat_id: ChatId,
    ) -> Result<Vec<tidebreak_core::ToolCallRecord>> {
        self.inner.list_tool_calls(chat_id).await
    }

    async fn get_setting(&self, key: &str) -> Result<Option<serde_json::Value>> {
        self.inner.get_setting(key).await
    }

    async fn set_setting(&self, key: &str, value: &serde_json::Value) -> Result<()> {
        self.inner.set_setting(key, value).await
    }

    async fn delete_setting(&self, key: &str) -> Result<()> {
        self.inner.delete_setting(key).await
    }

    async fn append_event(
        &self,
        chat_id: ChatId,
        event: &tidebreak_core::AgentEvent,
    ) -> Result<i64> {
        self.inner.append_event(chat_id, event).await
    }

    async fn list_events(
        &self,
        chat_id: ChatId,
        after: i64,
    ) -> Result<Vec<tidebreak_core::SequencedEvent>> {
        self.inner.list_events(chat_id, after).await
    }
}

// --- Mock Docker backend ------------------------------------------------------

/// A [`SandboxBackend`] that stands in for Docker.
///
/// `provision` starts the real in-container agent on a loopback listener with
/// no task — exactly as the image starts. The task only ever arrives in the
/// run-init frame the driver sends after attach, so task delivery is genuinely
/// testable: a driver that failed to send init leaves the agent parked and the
/// test times out, and the prompt the agent asks the host to complete proves
/// which task it actually received.
struct MockBackend {
    /// When set, `address` resolves here instead of the provisioned sandbox —
    /// used to point the driver at an unreachable port, or at a sandbox the
    /// test started itself for the reconcile path.
    address_override: Option<String>,
    /// The loopback address of the sandbox started at provision.
    address: Mutex<Option<String>>,
    provisions: AtomicUsize,
    destroys: AtomicUsize,
    /// While set, `destroy` refuses to confirm — the unconfirmed-teardown case.
    failing_destroys: std::sync::atomic::AtomicBool,
    /// Every live-tag set `reclaim_orphans` was asked to preserve.
    reclaim_live_sets:
        Mutex<Vec<std::collections::HashSet<tidebreak_sandbox_protocol::ids::SandboxTag>>>,
}

impl MockBackend {
    /// A backend that starts the real agent on provision, carrying whatever task
    /// the driver delivered.
    fn spawning() -> Arc<Self> {
        Arc::new(Self {
            address_override: None,
            address: Mutex::new(None),
            provisions: AtomicUsize::new(0),
            destroys: AtomicUsize::new(0),
            failing_destroys: std::sync::atomic::AtomicBool::new(false),
            reclaim_live_sets: Mutex::new(Vec::new()),
        })
    }

    /// A backend whose containers are never reachable at `base_url`.
    fn unreachable(base_url: String) -> Arc<Self> {
        Arc::new(Self {
            address_override: Some(base_url),
            address: Mutex::new(None),
            provisions: AtomicUsize::new(0),
            destroys: AtomicUsize::new(0),
            failing_destroys: std::sync::atomic::AtomicBool::new(false),
            reclaim_live_sets: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait]
impl SandboxBackend for MockBackend {
    async fn provision(
        &self,
        request: ProvisionRequest,
    ) -> std::result::Result<SandboxHandle, BackendError> {
        self.provisions.fetch_add(1, Ordering::SeqCst);
        if self.address_override.is_none() {
            // Start the sandbox exactly as the image does: with no task at all.
            // The agent waits for the run-init frame, so a driver that never
            // delivers one leaves the loop parked and the test times out — the
            // failure this models.
            *self.address.lock().unwrap() = Some(spawn_sandbox_agent().await);
        }
        Ok(SandboxHandle {
            reference: format!("mock-{}", request.run_id),
            tag: request.tag,
        })
    }

    async fn address(
        &self,
        _handle: &SandboxHandle,
    ) -> std::result::Result<SandboxAddress, BackendError> {
        let base_url = match &self.address_override {
            Some(base_url) => base_url.clone(),
            None => self
                .address
                .lock()
                .unwrap()
                .clone()
                .ok_or_else(|| BackendError::Unaddressable("not provisioned".to_owned()))?,
        };
        Ok(SandboxAddress {
            base_url,
            transport_secret: TransportSecret::new("test-secret"),
        })
    }

    async fn destroy(&self, _handle: &SandboxHandle) -> std::result::Result<(), BackendError> {
        if self.failing_destroys.load(Ordering::SeqCst) {
            return Err(BackendError::Teardown("destroy refused by test".to_owned()));
        }
        self.destroys.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn reclaim_orphans(
        &self,
        live_tags: &std::collections::HashSet<tidebreak_sandbox_protocol::ids::SandboxTag>,
    ) -> std::result::Result<Vec<SandboxHandle>, BackendError> {
        self.reclaim_live_sets
            .lock()
            .unwrap()
            .push(live_tags.clone());
        Ok(Vec::new())
    }
}

/// A backend that holds the orphan sweep after it receives the live-tag
/// snapshot, exposing obligations committed while that snapshot is in use.
struct HeldReclaimBackend {
    started: Notify,
    release: Notify,
}

impl HeldReclaimBackend {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            started: Notify::new(),
            release: Notify::new(),
        })
    }
}

#[async_trait]
impl SandboxBackend for HeldReclaimBackend {
    async fn provision(
        &self,
        _request: ProvisionRequest,
    ) -> std::result::Result<SandboxHandle, BackendError> {
        Err(BackendError::Provision("not used by test".to_owned()))
    }

    async fn address(
        &self,
        _handle: &SandboxHandle,
    ) -> std::result::Result<SandboxAddress, BackendError> {
        Err(BackendError::Unaddressable("not used by test".to_owned()))
    }

    async fn destroy(&self, _handle: &SandboxHandle) -> std::result::Result<(), BackendError> {
        Ok(())
    }

    async fn reclaim_orphans(
        &self,
        _live_tags: &std::collections::HashSet<SandboxTag>,
    ) -> std::result::Result<Vec<SandboxHandle>, BackendError> {
        self.started.notify_one();
        self.release.notified().await;
        Ok(Vec::new())
    }
}

/// A backend whose create has already produced an ambiguous tagged side effect
/// but whose `provision` future does not return until the test releases it.
/// Dropping the future models a runtime command that may keep running after its
/// caller stops awaiting it; the tag sweep is the only portable cleanup seam in
/// the backend contract when no handle was returned.
struct HeldProvisionBackend {
    started: Notify,
    release: Notify,
    provisions: AtomicUsize,
    returned: AtomicBool,
    dropped: Arc<AtomicBool>,
    orphans: Mutex<std::collections::HashSet<SandboxTag>>,
    reclaimed: Mutex<Vec<SandboxTag>>,
}

impl HeldProvisionBackend {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            started: Notify::new(),
            release: Notify::new(),
            provisions: AtomicUsize::new(0),
            returned: AtomicBool::new(false),
            dropped: Arc::new(AtomicBool::new(false)),
            orphans: Mutex::new(std::collections::HashSet::new()),
            reclaimed: Mutex::new(Vec::new()),
        })
    }
}

struct HeldProvisionDrop {
    dropped: Arc<AtomicBool>,
    completed: bool,
}

impl Drop for HeldProvisionDrop {
    fn drop(&mut self) {
        if !self.completed {
            self.dropped.store(true, Ordering::SeqCst);
        }
    }
}

#[async_trait]
impl SandboxBackend for HeldProvisionBackend {
    async fn provision(
        &self,
        request: ProvisionRequest,
    ) -> std::result::Result<SandboxHandle, BackendError> {
        self.provisions.fetch_add(1, Ordering::SeqCst);
        self.orphans.lock().unwrap().insert(request.tag);
        let mut drop_guard = HeldProvisionDrop {
            dropped: self.dropped.clone(),
            completed: false,
        };
        self.started.notify_one();
        self.release.notified().await;
        drop_guard.completed = true;
        self.returned.store(true, Ordering::SeqCst);
        Ok(SandboxHandle {
            reference: format!("held-{}", request.run_id),
            tag: request.tag,
        })
    }

    async fn address(
        &self,
        _handle: &SandboxHandle,
    ) -> std::result::Result<SandboxAddress, BackendError> {
        Err(BackendError::Unaddressable(
            "held provisioning has no address".to_owned(),
        ))
    }

    async fn destroy(&self, handle: &SandboxHandle) -> std::result::Result<(), BackendError> {
        self.orphans.lock().unwrap().remove(&handle.tag);
        Ok(())
    }

    async fn reclaim_orphans(
        &self,
        live_tags: &std::collections::HashSet<SandboxTag>,
    ) -> std::result::Result<Vec<SandboxHandle>, BackendError> {
        let mut orphans = self.orphans.lock().unwrap();
        let reclaimable = orphans
            .iter()
            .copied()
            .filter(|tag| !live_tags.contains(tag))
            .collect::<Vec<_>>();
        for tag in &reclaimable {
            orphans.remove(tag);
        }
        self.reclaimed
            .lock()
            .unwrap()
            .extend(reclaimable.iter().copied());
        Ok(reclaimable
            .into_iter()
            .map(|tag| SandboxHandle {
                reference: format!("held-{tag}"),
                tag,
            })
            .collect())
    }
}

// --- Store / admission fixture ------------------------------------------------

async fn store() -> (tempfile::TempDir, Arc<dyn Store>, Chat) {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let chat = Chat {
        id: ChatId::new(),
        project_id: None,
        title: Some("container".into()),
        model: Some("host-model".into()),
        permission_mode: None,
        network_policy: Default::default(),
        reasoning_effort: None,
        attachment_revision: 0,
        root_attachments: Vec::new(),
        created_at: chrono::Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();
    (dir, store, chat)
}

/// Admit one container-located sandbox child under the chat's running turn,
/// mirroring how a foreground turn admits a sandbox child — but at the container
/// execution location.
async fn admit_container_run(store: &Arc<dyn Store>, chat_id: ChatId, task: &str) -> AgentRunId {
    let turn_id = tidebreak_core::TurnId::new();
    store
        .accept_turn(turn_id, chat_id, "host-model", "delegate a container run")
        .await
        .unwrap();
    let lease = Uuid::new_v4();
    let now = chrono::Utc::now();
    let turn = store
        .claim_turn_run(lease, now, now + chrono::Duration::hours(1))
        .await
        .unwrap()
        .turn
        .expect("test turn should claim");
    let call = CallId::new();
    match store
        .admit_sandbox_container_agent_run(
            turn.id,
            call,
            task,
            lease,
            turn.steer_revision,
            AgentRun::MAX_CONCURRENCY_LIMIT,
            chrono::Utc::now(),
        )
        .await
        .unwrap()
        .expect("container admission should resolve")
    {
        AdmitSandboxAgentRunOutcome::Accepted { child, .. }
        | AdmitSandboxAgentRunOutcome::Existing { child, .. } => child.id,
        outcome => panic!("unexpected container admission: {outcome:?}"),
    }
}

/// Bind a loopback listener and serve the real in-container agent behind it: the
/// transport server against a fresh [`SandboxRun`], plus the agent loop that
/// dials model completions back over the reverse channel. Returns the bound
/// `http://` base URL.
async fn spawn_sandbox_agent() -> String {
    // The supervisor expects the same per-run secret the MockBackend hands the
    // driver from `address()`, so the driver's authenticated attach is accepted.
    let run = SandboxRun::new(
        [Capability::ModelInference],
        Some(TransportSecret::new("test-secret")),
    );
    let agent_run = run.clone();
    // The in-container tool surface is rooted at a workspace directory; give the
    // loop a private temp one that lives as long as the run.
    let workspace = tempfile::tempdir().unwrap();
    let workspace_path = workspace.path().to_path_buf();
    tokio::spawn(async move {
        let _workspace = workspace;
        // As in the image's entrypoint: the loop starts only once the host
        // delivers the run init.
        let init = agent_run.init().await;
        let _ = run_agent(agent_run, init.task, workspace_path).await;
    });
    spawn_sandbox_run(run).await
}

/// Serve a caller-controlled sandbox run over loopback without starting the
/// real agent loop. Lifecycle tests use this to race reverse calls and terminal
/// events at exact points while still exercising the production wire stack.
async fn spawn_sandbox_run(run: SandboxRun) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        while let Ok((stream, _peer)) = listener.accept().await {
            let run = run.clone();
            tokio::spawn(async move {
                let _ = serve_connection(stream, run).await;
            });
        }
    });
    base_url
}

/// A scoped-token issuer that records every mint and revoke, minting tokens
/// bounded by the deadline it is handed — or deliberately overrunning it, for
/// the driver's verification path.
struct RecordingTokenIssuer {
    /// `(run, deadline cap, minted expiry)` per mint, in order.
    minted: Mutex<Vec<(Uuid, u64, u64)>>,
    revoked: Mutex<Vec<Uuid>>,
    /// When true, the mint ignores the cap — the misbehaving issuer the
    /// driver must refuse rather than trust.
    overrun: bool,
}

impl RecordingTokenIssuer {
    fn honest() -> Arc<Self> {
        Arc::new(Self {
            minted: Mutex::new(Vec::new()),
            revoked: Mutex::new(Vec::new()),
            overrun: false,
        })
    }

    fn overrunning() -> Arc<Self> {
        Arc::new(Self {
            minted: Mutex::new(Vec::new()),
            revoked: Mutex::new(Vec::new()),
            overrun: true,
        })
    }
}

#[async_trait]
impl ScopedModelTokenIssuer for RecordingTokenIssuer {
    fn available(&self) -> bool {
        true
    }

    async fn mint(&self, run_id: Uuid, deadline_unix_secs: u64) -> Result<MintedScopedToken> {
        let now = chrono::Utc::now().timestamp().max(0).unsigned_abs();
        let expires_at_unix_secs = if self.overrun {
            deadline_unix_secs + 300
        } else {
            now.saturating_add(60).min(deadline_unix_secs)
        };
        self.minted
            .lock()
            .unwrap()
            .push((run_id, deadline_unix_secs, expires_at_unix_secs));
        Ok(MintedScopedToken {
            token: tidebreak_sandbox_protocol::init::ScopedModelToken::new(format!(
                "scoped-{run_id}"
            )),
            expires_at_unix_secs,
        })
    }

    async fn revoke(&self, run_id: Uuid) -> Result<()> {
        self.revoked.lock().unwrap().push(run_id);
        Ok(())
    }
}

fn fast_config() -> SandboxContainerRunConfig {
    SandboxContainerRunConfig {
        lease: Duration::from_secs(30),
        heartbeat: Duration::from_secs(5),
        durable_fence_interval: Duration::from_millis(250),
        dial_timeout: Duration::from_secs(2),
        reattach_attempts: 2,
        reattach_backoff: Duration::from_millis(10),
        provision_window: Duration::from_secs(120),
        max_concurrent_containers: 4,
        max_inference_operations: 24,
    }
}

fn fast_worker_config() -> SandboxContainerRunWorkerConfig {
    SandboxContainerRunWorkerConfig {
        idle_min: Duration::from_millis(10),
        idle_cap: Duration::from_millis(20),
        failure_delay: Duration::from_millis(10),
        maintenance_interval: Duration::from_millis(25),
        candidate_limit: 8,
        max_concurrency: 2,
    }
}

// --- Tests --------------------------------------------------------------------

/// The production service seam finds a durable queued container run without
/// being handed its id, claims it under the runner's cap, and drives the real
/// sandbox agent over loopback to its committed result.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn container_worker_service_drives_queued_work_over_loopback() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let (_dir, store, chat) = store().await;
        let run_id = admit_container_run(&store, chat.id, "driven by the service").await;
        let backend = MockBackend::spawning();
        let provider = Arc::new(ScriptedProvider::new(vec!["service result".to_owned()]));
        let worker = SandboxContainerRunWorker::new(
            store.clone(),
            backend.clone(),
            Arc::new(FixedResolver(provider)),
            Arc::new(Notify::new()),
            Arc::new(SandboxSteerGuard::default()),
            true,
            fast_config(),
            fast_worker_config(),
        )
        .expect("the explicitly enabled service is constructed");
        let task = tokio::spawn(worker.run());

        // Wait for the run's committed result and a discharged teardown
        // obligation. The service's maintenance cadence drives pending
        // teardowns too, so it can issue a directed destroy for the very handle
        // the driver is already tearing down — destroy is idempotent at the
        // backend and the sweep is built to run beside live drivers, so the
        // destroy call count is not a fixed number here. What the service owes
        // is that the container it provisioned is gone and its obligation is
        // discharged; the runner-level tests pin the exactly-once destroy on
        // the drive path, where nothing else is sweeping.
        loop {
            let run = store.get_agent_run(run_id).await.unwrap().unwrap();
            let torn_down = backend.destroys.load(Ordering::SeqCst) >= 1
                && store.list_sandbox_teardowns().await.unwrap().is_empty();
            if run.status == AgentRunStatus::Completed && torn_down {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        // Exactly one container was created for the run, whatever the sweep did
        // alongside the driver's own teardown.
        assert_eq!(backend.provisions.load(Ordering::SeqCst), 1);
    })
    .await
    .expect("service drive completed within its time bound");
}

/// A teardown created after the service's startup pass is completed by a later
/// maintenance cadence, proving teardown recovery is periodic rather than a
/// one-shot boot action.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn container_worker_service_cadence_completes_pending_teardown() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (_dir, store, _chat) = store().await;
        let backend = MockBackend::spawning();
        let provider = Arc::new(ScriptedProvider::new(Vec::new()));
        let worker = SandboxContainerRunWorker::new(
            store.clone(),
            backend.clone(),
            Arc::new(FixedResolver(provider)),
            Arc::new(Notify::new()),
            Arc::new(SandboxSteerGuard::default()),
            true,
            fast_config(),
            fast_worker_config(),
        )
        .expect("the explicitly enabled service is constructed");
        let task = tokio::spawn(worker.run());

        // Let the immediate startup maintenance pass finish before creating the
        // obligation, so only a subsequent cadence can observe it.
        loop {
            if !backend.reclaim_live_sets.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        let run_id = Uuid::new_v4();
        store
            .begin_sandbox_provision(
                run_id,
                &SandboxTag::new().to_string(),
                chrono::Utc::now() + chrono::Duration::seconds(60),
                tidebreak_core::SandboxAdmissionMode::AttachedOnly,
            )
            .await
            .unwrap();
        assert!(store
            .commit_sandbox_provision_handle(run_id, "pending-service-teardown")
            .await
            .unwrap());
        store
            .enqueue_sandbox_teardown(run_id)
            .await
            .unwrap()
            .expect("the committed handle becomes a teardown obligation");

        loop {
            if store.list_sandbox_teardowns().await.unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(backend.destroys.load(Ordering::SeqCst), 1);
    })
    .await
    .expect("maintenance cadence completed within its time bound");
}

/// The whole stack over loopback: admit a container run, drive it with the real
/// in-container agent loop running a sandbox filesystem tool and dialing the host
/// for model inference, and assert the host committed the result exactly once,
/// proxied each model step through the resolver, delivered the run's ACTUAL task,
/// and tore the container down.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn drives_a_container_run_end_to_end_over_loopback() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let (_dir, store, chat) = store().await;
        let task = "count the words in this delegated sentence";
        let run_id = admit_container_run(&store, chat.id, task).await;

        // The backend starts the sandbox on whatever task the driver delivered,
        // exactly as Docker starts the container from its environment.
        let backend = MockBackend::spawning();
        // Step 1: write a workspace file. Step 2: the final answer.
        let provider = Arc::new(ScriptedProvider::new(vec![
            "use-tool:write_file:{\"path\":\"note.txt\",\"content\":\"delegated\"}".to_owned(),
            "the text has three words".to_owned(),
        ]));
        let resolver = Arc::new(FixedResolver(provider.clone()));

        let runner =
            SandboxContainerRunner::new(store.clone(), backend.clone(), resolver, fast_config());
        let outcome = runner
            .drive(run_id)
            .await
            .expect("driving succeeds")
            .expect("the container run is claimable");
        assert_eq!(outcome, SandboxContainerRunOutcome::Completed(run_id));

        // The host committed the container's final result exactly once, through
        // the fenced result path.
        let committed = store.get_agent_run(run_id).await.unwrap().unwrap();
        assert_eq!(committed.status, AgentRunStatus::Completed);

        // Both model steps were proxied through the host resolver (no model
        // credential in the container), and the op-log recorded each exactly once.
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            2,
            "each model step should proxy through the host exactly once"
        );

        // The run's ACTUAL delegated task reached the container over the
        // run-init frame and is what the container asked the host to reason
        // about.
        assert!(
            provider.first_prompt().contains(task),
            "the container must work on the delegated task, got prompt: {}",
            provider.first_prompt()
        );

        // The container was provisioned once and torn down.
        assert_eq!(backend.provisions.load(Ordering::SeqCst), 1);
        assert_eq!(backend.destroys.load(Ordering::SeqCst), 1);

        // The fail-closed detached gate: no precondition holds on a local
        // container, so the durable admission decision the driver recorded is
        // attached-only (issue #824).
        let provision = store
            .get_sandbox_provision(*run_id.as_uuid())
            .await
            .unwrap()
            .expect("the run has a provisioning record");
        assert_eq!(
            provision.admission,
            tidebreak_core::SandboxAdmissionMode::AttachedOnly,
            "a local container run must be admitted attached-only"
        );
    })
    .await
    .expect("test completed within its time bound");
}

/// The admission decision is a durable fact on the provisioning record: what
/// was recorded is what reads back, and recovery derives the run's admission
/// from the record rather than re-deciding — so a crash can never upgrade a
/// run to detached, and a detached admission survives the host that made it.
#[tokio::test(flavor = "multi_thread")]
async fn the_admission_decision_is_durable_on_the_provisioning_record() {
    let (_dir, store, _chat) = store().await;
    let run_uuid = Uuid::new_v4();
    assert!(matches!(
        store
            .begin_sandbox_provision(
                run_uuid,
                &SandboxTag::new().to_string(),
                chrono::Utc::now() + chrono::Duration::seconds(600),
                tidebreak_core::SandboxAdmissionMode::Detached,
            )
            .await
            .unwrap(),
        tidebreak_core::BeginSandboxProvisionOutcome::Started
    ));
    let record = store
        .get_sandbox_provision(run_uuid)
        .await
        .unwrap()
        .expect("the record exists");
    assert_eq!(
        record.admission,
        tidebreak_core::SandboxAdmissionMode::Detached
    );

    // A second begin for the same run — the crash-recovery path — observes
    // the recorded decision, not its own argument.
    let tidebreak_core::BeginSandboxProvisionOutcome::Existing(existing) = store
        .begin_sandbox_provision(
            run_uuid,
            &SandboxTag::new().to_string(),
            chrono::Utc::now() + chrono::Duration::seconds(600),
            tidebreak_core::SandboxAdmissionMode::AttachedOnly,
        )
        .await
        .unwrap()
    else {
        panic!("the second begin must observe the existing record");
    };
    assert_eq!(
        existing.admission,
        tidebreak_core::SandboxAdmissionMode::Detached,
        "recovery reads the durable decision, never re-decides"
    );
}

/// The scoped-token contract of #824 slice 2, at the driver's mint seam: an
/// attached-only run mints nothing; a detached run's token is capped by the
/// run deadline, with an issuer that overruns the cap refused and revoked;
/// and every path that cannot produce a valid token fails closed — including
/// the default (gateway) issuer, which cannot mint today.
#[tokio::test(flavor = "multi_thread")]
async fn a_detached_scoped_token_is_capped_by_the_run_deadline_and_fails_closed() {
    let (_dir, store, _chat) = store().await;
    let backend = MockBackend::spawning();
    let resolver = Arc::new(FixedResolver(Arc::new(ScriptedProvider::new(vec![]))));
    let deadline = chrono::Utc::now().timestamp().max(0).unsigned_abs() + 600;

    let issuer = RecordingTokenIssuer::honest();
    let runner = SandboxContainerRunner::new(
        store.clone(),
        backend.clone(),
        resolver.clone(),
        fast_config(),
    )
    .with_token_issuer(issuer.clone());

    // An attached-only run never mints: the host is its model proxy.
    assert!(runner
        .scoped_token_for(
            Uuid::new_v4(),
            tidebreak_core::SandboxAdmissionMode::AttachedOnly,
            deadline,
        )
        .await
        .unwrap()
        .is_none());
    assert!(issuer.minted.lock().unwrap().is_empty());

    // A detached run's token expires no later than the run deadline.
    let run = Uuid::new_v4();
    runner
        .scoped_token_for(
            run,
            tidebreak_core::SandboxAdmissionMode::Detached,
            deadline,
        )
        .await
        .unwrap()
        .expect("a detached run receives a token");
    let minted = issuer.minted.lock().unwrap().clone();
    assert_eq!(minted.len(), 1);
    assert!(
        minted[0].2 <= deadline,
        "token lifetime must be capped by the run deadline"
    );

    // An issuer whose token would outlive the run is refused, and whatever it
    // minted is revoked before the refusal.
    let overrunning = RecordingTokenIssuer::overrunning();
    let refusing = SandboxContainerRunner::new(
        store.clone(),
        backend.clone(),
        resolver.clone(),
        fast_config(),
    )
    .with_token_issuer(overrunning.clone());
    let run = Uuid::new_v4();
    assert!(refusing
        .scoped_token_for(
            run,
            tidebreak_core::SandboxAdmissionMode::Detached,
            deadline
        )
        .await
        .is_err());
    assert_eq!(overrunning.revoked.lock().unwrap().as_slice(), &[run]);

    // No absolute deadline to cap by refuses rather than minting unbounded.
    assert!(runner
        .scoped_token_for(
            Uuid::new_v4(),
            tidebreak_core::SandboxAdmissionMode::Detached,
            0
        )
        .await
        .is_err());

    // The default issuer — the gateway, which has no run-scoped mint API —
    // fails closed rather than delivering any credential.
    let default_runner =
        SandboxContainerRunner::new(store.clone(), backend, resolver, fast_config());
    assert!(default_runner
        .scoped_token_for(
            Uuid::new_v4(),
            tidebreak_core::SandboxAdmissionMode::Detached,
            deadline
        )
        .await
        .is_err());
}

/// The admission gate's `scoped_model_token_available` input is the issuer's
/// real availability, not a constant: the default (gateway) issuer reads
/// unavailable and the evaluated denial names the missing token, while an
/// issuer that can mint flips exactly that input.
#[tokio::test(flavor = "multi_thread")]
async fn the_gate_input_reflects_the_issuers_availability() {
    let (_dir, store, _chat) = store().await;
    let backend = MockBackend::spawning();
    let resolver = Arc::new(FixedResolver(Arc::new(ScriptedProvider::new(vec![]))));

    let default_runner = SandboxContainerRunner::new(
        store.clone(),
        backend.clone(),
        resolver.clone(),
        fast_config(),
    );
    let preconditions = default_runner.detached_preconditions();
    assert!(!preconditions.scoped_model_token_available);
    let DetachedAdmission::Denied(denials) = evaluate_detached_admission(preconditions) else {
        panic!("a local container run must be denied detached admission");
    };
    assert!(denials.contains(&DetachedAdmissionDenial::NoScopedModelToken));

    let minting = SandboxContainerRunner::new(store.clone(), backend, resolver, fast_config())
        .with_token_issuer(RecordingTokenIssuer::honest());
    assert!(
        minting
            .detached_preconditions()
            .scoped_model_token_available,
        "an issuer that can mint must read as available, truthfully"
    );
}

/// The reaper-shaped revocation path: a run whose own driver never revoked —
/// a lapsed provisioning intent, or a teardown obligation left by a reaped or
/// unattached-cancelled run — has its scoped token revoked by the sweep.
#[tokio::test(flavor = "multi_thread")]
async fn the_sweep_revokes_tokens_for_lapsed_and_reaped_runs() {
    let (_dir, store, _chat) = store().await;
    let lapsed_run = Uuid::new_v4();
    let reaped_run = Uuid::new_v4();
    store
        .begin_sandbox_provision(
            lapsed_run,
            &SandboxTag::new().to_string(),
            chrono::Utc::now() - chrono::Duration::seconds(1),
            tidebreak_core::SandboxAdmissionMode::AttachedOnly,
        )
        .await
        .unwrap();
    store
        .begin_sandbox_provision(
            reaped_run,
            &SandboxTag::new().to_string(),
            chrono::Utc::now() + chrono::Duration::seconds(600),
            tidebreak_core::SandboxAdmissionMode::AttachedOnly,
        )
        .await
        .unwrap();
    store.enqueue_sandbox_teardown(reaped_run).await.unwrap();

    let issuer = RecordingTokenIssuer::honest();
    let runner = SandboxContainerRunner::new(
        store.clone(),
        MockBackend::spawning(),
        Arc::new(FixedResolver(Arc::new(ScriptedProvider::new(vec![])))),
        fast_config(),
    )
    .with_token_issuer(issuer.clone());
    runner.sweep().await.expect("the sweep succeeds");

    let revoked = issuer.revoked.lock().unwrap().clone();
    assert!(
        revoked.contains(&lapsed_run),
        "a lapsed intent's token is revoked by the sweep"
    );
    assert!(
        revoked.contains(&reaped_run),
        "a reaped run's teardown obligation carries its revocation"
    );
}

/// An agent loop that ends WITHOUT submitting a result — it exhausts its step
/// budget — still terminalizes the run and tears the container down.
///
/// This is the container-leak case a reachable-but-resultless container creates:
/// the supervisor keeps serving after the agent loop returns, so a driver that
/// only watched for a result would wait on the open socket forever, never tear
/// down, and leave the run to be reaped. The agent's terminal failure event is
/// what closes it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn terminalizes_and_tears_down_when_the_agent_loop_ends_without_a_result() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let (_dir, store, chat) = store().await;
        let run_id = admit_container_run(&store, chat.id, "never finishes").await;

        let backend = MockBackend::spawning();
        // Every completion is another tool directive, so the loop never submits a
        // final answer and exhausts MAX_STEPS.
        let provider = Arc::new(ScriptedProvider::new(vec![
            "use-tool:write_file:{\"path\":\"note.txt\",\"content\":\"a\"}".to_owned();
            16
        ]));
        let resolver = Arc::new(FixedResolver(provider.clone()));

        let runner =
            SandboxContainerRunner::new(store.clone(), backend.clone(), resolver, fast_config());
        let outcome = runner
            .drive(run_id)
            .await
            .expect("driving succeeds")
            .expect("the container run is claimable");
        assert_eq!(outcome, SandboxContainerRunOutcome::Failed(run_id));

        let failed = store.get_agent_run(run_id).await.unwrap().unwrap();
        assert_eq!(failed.status, AgentRunStatus::Failed);
        assert_eq!(
            failed.last_error_code.as_deref(),
            Some("sandbox_agent_failed"),
            "a loop that ended without a result is an agent failure, not a transport one"
        );
        // The container did not leak: teardown ran even though no result arrived.
        assert_eq!(backend.destroys.load(Ordering::SeqCst), 1);
    })
    .await
    .expect("test completed within its time bound");
}

/// The host stops answering model inference at the run's spend budget, no matter
/// how many reverse calls the sandbox issues.
///
/// The in-container step limit is enforced by untrusted code, so it bounds
/// nothing; this is the host-side cap #920 requires before anything routes to
/// the container location. The refusal is non-retryable, so the sandbox's failed
/// model step terminalizes the run and the container is torn down — and the
/// provider must have been asked for exactly the budgeted number of completions,
/// not one more.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stops_proxying_inference_at_the_runs_spend_budget() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let (_dir, store, chat) = store().await;
        let run_id = admit_container_run(&store, chat.id, "spends forever").await;

        let backend = MockBackend::spawning();
        // Every completion is another tool directive: left alone, the loop would
        // take all of its 8 in-container steps. The host's budget of 2 must cut
        // it off first.
        let provider = Arc::new(ScriptedProvider::new(vec![
            "use-tool:write_file:{\"path\":\"note.txt\",\"content\":\"a\"}".to_owned();
            16
        ]));
        let resolver = Arc::new(FixedResolver(provider.clone()));

        let runner = SandboxContainerRunner::new(
            store.clone(),
            backend.clone(),
            resolver,
            SandboxContainerRunConfig {
                max_inference_operations: 2,
                ..fast_config()
            },
        );
        let outcome = runner
            .drive(run_id)
            .await
            .expect("driving succeeds")
            .expect("the container run is claimable");
        assert_eq!(outcome, SandboxContainerRunOutcome::Failed(run_id));

        let failed = store.get_agent_run(run_id).await.unwrap().unwrap();
        assert_eq!(failed.status, AgentRunStatus::Failed);
        assert_eq!(
            failed.last_error_code.as_deref(),
            Some("sandbox_agent_failed")
        );

        // The user's credentials were spent exactly the budgeted number of
        // times; the third request was refused before reaching the provider.
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            2,
            "the host must not answer inference past the run's budget"
        );
        // The refusal closed the run rather than leaking the container.
        assert_eq!(backend.destroys.load(Ordering::SeqCst), 1);
    })
    .await
    .expect("test completed within its time bound");
}

/// A retryable provider attempt reserved by a prior host lifetime still
/// consumes the run's hard spend budget after the container is reattached.
/// Completed model-step accounting deliberately excludes this zero-observation
/// failure, so recovery must seed the cap from the durable reverse-operation
/// claims rather than from `model_steps` alone.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recovered_drive_keeps_failed_provider_attempts_in_the_spend_budget() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let (_dir, store, chat) = store().await;
        let run_id = admit_container_run(&store, chat.id, "resume a partly spent run").await;

        let prior_operation = Uuid::new_v4();
        assert_eq!(
            store
                .claim_operation(
                    *run_id.as_uuid(),
                    prior_operation,
                    b"prior retryable model attempt",
                    true,
                    Uuid::new_v4(),
                )
                .await
                .unwrap(),
            tidebreak_core::OperationClaimOutcome::Fresh
        );
        store
            .fail_operation(
                *run_id.as_uuid(),
                prior_operation,
                b"provider stream failed before observation",
            )
            .await
            .unwrap();

        let backend = MockBackend::spawning();
        let provider = Arc::new(ScriptedProvider::new(vec![
            "use-tool:write_file:{\"path\":\"note.txt\",\"content\":\"a\"}".to_owned();
            4
        ]));
        let runner = SandboxContainerRunner::new(
            store.clone(),
            backend.clone(),
            Arc::new(FixedResolver(provider.clone())),
            SandboxContainerRunConfig {
                max_inference_operations: 2,
                ..fast_config()
            },
        );

        let outcome = runner
            .drive(run_id)
            .await
            .expect("driving succeeds")
            .expect("the container run is claimable");
        assert_eq!(outcome, SandboxContainerRunOutcome::Failed(run_id));
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            1,
            "the prior durable failed attempt leaves only one provider call"
        );
        assert_eq!(backend.destroys.load(Ordering::SeqCst), 1);
    })
    .await
    .expect("test completed within its time bound");
}

/// The driver keeps the run's lease live while the container works, so a run that
/// outlives one lease period is not reaped mid-flight.
///
/// The provider gate holds the real sandbox in a live model call while the test
/// observes the durable lease. The run must remain `running` and its expiry must
/// move forward before the model call is released.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn heartbeats_the_lease_so_a_long_run_is_not_reaped() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let (_dir, store, chat) = store().await;
        let run_id = admit_container_run(&store, chat.id, "takes a while").await;

        let backend = MockBackend::spawning();
        let gate = Arc::new(StepGate::default());
        // Hold the first completion until the durable heartbeat is visible.
        // The lease must outlast provision+attach — heartbeats only start once
        // the container is driving — so a short lease expires under CI
        // contention before the first tick and this loop never observes an
        // extension. A short heartbeat still proves the expiry moved without
        // sleeping through a full lease period.
        let provider = Arc::new(ScriptedProvider::gated(
            vec![
                "use-tool:write_file:{\"path\":\"note.txt\",\"content\":\"a b\"}".to_owned(),
                "done".to_owned(),
            ],
            gate.clone(),
        ));
        let resolver = Arc::new(FixedResolver(provider.clone()));

        let runner = SandboxContainerRunner::new(
            store.clone(),
            backend.clone(),
            resolver,
            SandboxContainerRunConfig {
                heartbeat: Duration::from_millis(100),
                ..fast_config()
            },
        );

        let drive = tokio::spawn(async move { runner.drive(run_id).await });

        gate.started.notified().await;
        let initial = store
            .get_agent_run(run_id)
            .await
            .unwrap()
            .unwrap()
            .lease_expires_at
            .expect("a claimed run has a lease expiry");

        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        let extended = loop {
            tokio::time::sleep(Duration::from_millis(25)).await;
            let run = store.get_agent_run(run_id).await.unwrap().unwrap();
            assert_eq!(
                run.status,
                AgentRunStatus::Running,
                "the run must stay live while its model call is held"
            );
            let expiry = run
                .lease_expires_at
                .expect("a running claimed run keeps a lease expiry");
            if expiry > initial {
                break expiry;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "lease expiry {expiry} did not move past {initial} while the model call was held"
            );
        };
        assert!(extended > initial);

        gate.release.notify_one();

        let outcome = drive
            .await
            .unwrap()
            .expect("driving succeeds")
            .expect("the container run is claimable");
        // The run completed normally rather than being reaped out from under the
        // still-working container.
        assert_eq!(outcome, SandboxContainerRunOutcome::Completed(run_id));
    })
    .await
    .expect("test completed within its time bound");
}

/// A container run is exempt from the in-process lease reaper: its lease
/// expiring does not terminalize it, because no in-process worker holds it and
/// the container may still be working and spending.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_in_process_reaper_leaves_an_expired_container_lease_alone() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (_dir, store, chat) = store().await;
        let run_id = admit_container_run(&store, chat.id, "reaper bait").await;

        // Claim with a lease that expires immediately, then let the in-process
        // scheduler scan. Its lease reaper would otherwise fail this run (a
        // container run has max_attempts = 1, so attempt_count >= max_attempts
        // the moment it is claimed).
        let lease = Uuid::new_v4();
        store
            .claim_container_agent_run(run_id, lease, chrono::Duration::milliseconds(1), 4)
            .await
            .unwrap()
            .expect("the container claim should pick up the queued run");
        tokio::time::sleep(Duration::from_millis(50)).await;

        let _ = store
            .claim_agent_run(Uuid::new_v4(), chrono::Duration::minutes(5), 4, 4)
            .await
            .unwrap();

        let after = store.get_agent_run(run_id).await.unwrap().unwrap();
        assert_eq!(
            after.status,
            AgentRunStatus::Running,
            "the in-process reaper must not terminalize a container run on lease expiry"
        );
    })
    .await
    .expect("test completed within its time bound");
}

/// A model-inference call re-issued after a reconnect is answered from the
/// durable op-log, not executed a second time: the host proxy runs once and the
/// re-issue replays its recorded completion. This is the reverse-RPC exactly-once
/// guarantee the reattachment path depends on, at the seam this slice adds
/// (the host proxy over the durable operation store).
#[tokio::test]
async fn cancelled_host_model_proxy_refuses_before_provider_egress() {
    let provider = Arc::new(ScriptedProvider::new(vec!["must not execute".to_owned()]));
    let cancel = CancelToken::new();
    cancel.cancel();
    let proxy = HostModelProxy {
        resolver: Arc::new(FixedResolver(provider.clone())),
        cancel,
        lease_guard: None,
        config: AgentConfig {
            model: "host-model".to_owned(),
            ..AgentConfig::default()
        },
        spent: AtomicU32::new(0),
        budget: 24,
        accounting: None,
        observed: HostModelObservedAccounting::default(),
    };

    let response = proxy
        .respond(ReverseRequest::ModelInference(ModelInferenceParams {
            prompt: "post-cancellation request".to_owned(),
        }))
        .await;
    match response {
        Response::Error(error) => assert_eq!(error.code, ErrorCode::Cancelled),
        other => panic!("cancelled proxy returned {other:?}"),
    }
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        0,
        "a cancelled container reverse request must not start provider egress"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn host_model_proxy_answers_a_reissued_inference_from_the_op_log() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (_dir, store, _chat) = store().await;
        let run_id = RunId::new();
        let provider = Arc::new(ScriptedProvider::new(vec!["the-answer".to_owned()]));
        let host = CapabilityHost::new(
            GrantSet::new(
                RunProvenance {
                    run_id,
                    provider: "test".to_owned(),
                },
                [Capability::ModelInference],
            ),
            Arc::new(HostModelProxy {
                resolver: Arc::new(FixedResolver(provider.clone())),
                cancel: CancelToken::new(),
                lease_guard: None,
                config: AgentConfig {
                    model: "host-model".to_owned(),
                    ..AgentConfig::default()
                },
                spent: AtomicU32::new(0),
                budget: 24,
                accounting: None,
                observed: HostModelObservedAccounting::default(),
            }),
            Arc::new(DurableOperationStore::new(store.clone(), run_id)),
        );

        let operation_id = OperationId::new();
        let request = ReverseRequest::ModelInference(ModelInferenceParams {
            prompt: "one step".to_owned(),
        });
        let envelope = || ReverseEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: tidebreak_sandbox_protocol::ids::RequestId::new(),
            operation_id,
            request: request.clone(),
        };

        let first = host.dispatch(envelope()).wait().await;
        // A re-issue with the same operation identity (a reconnect) — a fresh
        // RequestId, the same OperationId.
        let replay = host.dispatch(envelope()).wait().await;

        let expect = Response::Ok(ReverseResult::ModelInference(
            tidebreak_sandbox_protocol::reverse::ModelInferenceResult {
                completion: "the-answer".to_owned(),
            },
        ));
        assert_eq!(first, expect);
        assert_eq!(
            replay, expect,
            "the re-issue must replay the recorded answer"
        );
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            1,
            "a re-issued inference must not spend a second model call"
        );
    })
    .await
    .expect("test completed within its time bound");
}

/// A container that never becomes reachable is failed terminally after the
/// reattach budget, and its teardown obligation is still driven to completion —
/// a sandbox-resident run has exactly one attempt and is never re-executed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fails_terminally_and_tears_down_when_the_container_is_unreachable() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let (_dir, store, chat) = store().await;
        let run_id = admit_container_run(&store, chat.id, "unreachable").await;

        // A loopback port with nothing listening: every dial fails, so the driver
        // exhausts its reattach budget and fails the run.
        let dead = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let dead_addr = dead.local_addr().unwrap();
        drop(dead);
        let backend = MockBackend::unreachable(format!("http://{dead_addr}"));
        let resolver = Arc::new(FixedResolver(Arc::new(ScriptedProvider::new(vec![]))));

        let issuer = RecordingTokenIssuer::honest();
        let runner =
            SandboxContainerRunner::new(store.clone(), backend.clone(), resolver, fast_config())
                .with_token_issuer(issuer.clone());
        let outcome = runner
            .drive(run_id)
            .await
            .expect("driving succeeds")
            .expect("the container run is claimable");
        assert_eq!(outcome, SandboxContainerRunOutcome::Failed(run_id));

        let failed = store.get_agent_run(run_id).await.unwrap().unwrap();
        assert_eq!(failed.status, AgentRunStatus::Failed);
        // The teardown obligation was driven even though the run failed.
        assert_eq!(backend.destroys.load(Ordering::SeqCst), 1);
        // Terminalization revoked the run's scoped token along the same path.
        assert_eq!(
            issuer.revoked.lock().unwrap().as_slice(),
            &[*run_id.as_uuid()]
        );
    })
    .await
    .expect("test completed within its time bound");
}

/// Admission routes a container run to the container execution location: the
/// in-process scheduler leaves it, and only the container claim picks it up.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admission_routes_to_the_container_location_not_the_in_process_scheduler() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (_dir, store, chat) = store().await;
        let run_id = admit_container_run(&store, chat.id, "routed").await;

        let admitted = store.get_agent_run(run_id).await.unwrap().unwrap();
        assert_eq!(
            admitted.execution_location,
            AgentRunExecutionLocation::Container
        );
        assert_eq!(admitted.status, AgentRunStatus::Queued);
        // One attempt only: the run tier's retry machinery does not apply.
        assert_eq!(admitted.max_attempts, 1);

        // The in-process scheduler must not claim a container run.
        assert!(
            store
                .claim_agent_run(Uuid::new_v4(), chrono::Duration::minutes(5), 4, 4)
                .await
                .unwrap()
                .is_none(),
            "the in-process scheduler must leave a container run for the driver"
        );

        // The container claim transitions it to running under a lease.
        let lease = Uuid::new_v4();
        let claimed = store
            .claim_container_agent_run(run_id, lease, chrono::Duration::minutes(5), 4)
            .await
            .unwrap()
            .expect("the container claim should pick up the queued run");
        assert_eq!(claimed.id, run_id);
        assert_eq!(claimed.status, AgentRunStatus::Running);
        assert_eq!(claimed.lease_token, Some(lease));

        // Re-claiming with the same token recovers the same live claim, never a
        // second attempt.
        let reclaimed = store
            .claim_container_agent_run(run_id, lease, chrono::Duration::minutes(5), 4)
            .await
            .unwrap()
            .expect("reusing the token recovers the live claim");
        assert_eq!(reclaimed.id, run_id);
        assert_eq!(reclaimed.attempt_count, claimed.attempt_count);
    })
    .await
    .expect("test completed within its time bound");
}

// --- Durable provisioning records (issue #920) --------------------------------

/// The commit predicate: a handle commit that arrives after the window lapsed
/// finds the intent already disowned and must not resurrect it — the driver
/// that holds the late container destroys it instead of running on it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_lapsed_intent_refuses_a_late_handle_commit() {
    let (_dir, store, _chat) = store().await;
    let run_uuid = Uuid::new_v4();
    let tag = SandboxTag::new();
    let expired = chrono::Utc::now() - chrono::Duration::seconds(1);
    assert!(matches!(
        store
            .begin_sandbox_provision(
                run_uuid,
                &tag.to_string(),
                expired,
                tidebreak_core::SandboxAdmissionMode::AttachedOnly
            )
            .await
            .unwrap(),
        tidebreak_core::BeginSandboxProvisionOutcome::Started
    ));

    let lapsed = store
        .lapse_sandbox_provisions(chrono::Utc::now())
        .await
        .unwrap();
    assert_eq!(lapsed.len(), 1);
    assert_eq!(lapsed[0].run_id, run_uuid);

    // The create returns late: its commit must lose.
    assert!(!store
        .commit_sandbox_provision_handle(run_uuid, "late-container")
        .await
        .unwrap());
    // The disowned intent owes a teardown, and its tag is no longer live.
    assert_eq!(store.list_sandbox_teardowns().await.unwrap().len(), 1);
    assert!(store.live_sandbox_tags().await.unwrap().is_empty());
}

/// The sweep converges a crash between provision and handle commit: the lapsed
/// intent is disowned, the tag sweep is asked to preserve only live tags, and
/// the handle-less obligation completes once the backend proves nothing outside
/// them remains. An unlapsed intent stays live throughout, so the sweep can
/// never race a slow in-flight create.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_sweep_reclaims_a_lapsed_intent_and_preserves_live_ones() {
    let (_dir, store, _chat) = store().await;
    let dead_tag = SandboxTag::new();
    let live_tag = SandboxTag::new();
    let expired = chrono::Utc::now() - chrono::Duration::seconds(1);
    let open = chrono::Utc::now() + chrono::Duration::seconds(600);
    store
        .begin_sandbox_provision(
            Uuid::new_v4(),
            &dead_tag.to_string(),
            expired,
            tidebreak_core::SandboxAdmissionMode::AttachedOnly,
        )
        .await
        .unwrap();
    store
        .begin_sandbox_provision(
            Uuid::new_v4(),
            &live_tag.to_string(),
            open,
            tidebreak_core::SandboxAdmissionMode::AttachedOnly,
        )
        .await
        .unwrap();

    let backend = MockBackend::spawning();
    let runner = SandboxContainerRunner::new(
        store.clone(),
        backend.clone(),
        Arc::new(FixedResolver(Arc::new(ScriptedProvider::new(vec![])))),
        fast_config(),
    );
    runner.sweep().await.expect("the sweep succeeds");

    let live_sets = backend.reclaim_live_sets.lock().unwrap().clone();
    assert_eq!(live_sets.len(), 1);
    assert!(
        live_sets[0].contains(&live_tag),
        "an unlapsed intent's tag must stay live through the tag sweep"
    );
    assert!(
        !live_sets[0].contains(&dead_tag),
        "a lapsed intent's tag must be reclaimable"
    );
    // The obligation completed under the backend's nothing-remains guarantee.
    assert!(store.list_sandbox_teardowns().await.unwrap().is_empty());
    assert_eq!(
        store.live_sandbox_tags().await.unwrap(),
        vec![live_tag.to_string()]
    );
}

/// A teardown committed after the sweep freezes its live-tag view must retain
/// its retry obligation. Its tag may have been preserved as live by that view,
/// so only obligations captured before reclamation are safe to complete.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_obligation_created_during_orphan_reclamation_survives_for_the_next_sweep() {
    let (_dir, store, _chat) = store().await;
    let prior_run = Uuid::new_v4();
    store
        .begin_sandbox_provision(
            prior_run,
            &SandboxTag::new().to_string(),
            chrono::Utc::now() + chrono::Duration::seconds(60),
            tidebreak_core::SandboxAdmissionMode::AttachedOnly,
        )
        .await
        .unwrap();
    store
        .enqueue_sandbox_teardown(prior_run)
        .await
        .unwrap()
        .expect("the prior intent becomes a teardown obligation");

    let backend = HeldReclaimBackend::new();
    let runner = Arc::new(SandboxContainerRunner::new(
        store.clone(),
        backend.clone(),
        Arc::new(FixedResolver(Arc::new(ScriptedProvider::new(vec![])))),
        fast_config(),
    ));
    let sweep = tokio::spawn({
        let runner = runner.clone();
        async move { runner.sweep().await }
    });
    backend.started.notified().await;

    let late_run = Uuid::new_v4();
    store
        .begin_sandbox_provision(
            late_run,
            &SandboxTag::new().to_string(),
            chrono::Utc::now() + chrono::Duration::seconds(60),
            tidebreak_core::SandboxAdmissionMode::AttachedOnly,
        )
        .await
        .unwrap();
    assert!(store
        .commit_sandbox_provision_handle(late_run, "late-container")
        .await
        .unwrap());
    store
        .enqueue_sandbox_teardown(late_run)
        .await
        .unwrap()
        .expect("the late handle becomes a teardown obligation");

    backend.release.notify_one();
    sweep.await.unwrap().unwrap();

    let remaining = store.list_sandbox_teardowns().await.unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].run_id, late_run);
}

/// An unconfirmed teardown outlives the driver: the obligation is persisted at
/// the end of the drive, and the next sweep's directed destroy completes it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unconfirmed_teardown_is_redriven_by_the_sweep() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let (_dir, store, chat) = store().await;
        let run_id = admit_container_run(&store, chat.id, "clean up after me").await;
        let backend = MockBackend::spawning();
        backend.failing_destroys.store(true, Ordering::SeqCst);
        let provider = Arc::new(ScriptedProvider::new(vec![]));
        let runner = SandboxContainerRunner::new(
            store.clone(),
            backend.clone(),
            Arc::new(FixedResolver(provider)),
            fast_config(),
        );
        let outcome = runner
            .drive(run_id)
            .await
            .expect("driving succeeds")
            .expect("the container run is claimable");
        assert_eq!(outcome, SandboxContainerRunOutcome::Completed(run_id));
        // The destroy never confirmed, so the obligation survived the drive.
        assert_eq!(backend.destroys.load(Ordering::SeqCst), 0);
        assert_eq!(store.list_sandbox_teardowns().await.unwrap().len(), 1);

        backend.failing_destroys.store(false, Ordering::SeqCst);
        runner.sweep().await.expect("the sweep succeeds");
        assert_eq!(backend.destroys.load(Ordering::SeqCst), 1);
        assert!(store.list_sandbox_teardowns().await.unwrap().is_empty());
    })
    .await
    .expect("test completed within its time bound");
}

/// A committed handle from a prior interrupted attempt is reconciled: the
/// driver attaches to the container that already exists instead of provisioning
/// a second one for the same single-attempt run.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_committed_handle_is_reconciled_not_reprovisioned() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let (_dir, store, chat) = store().await;
        let task = "resume where the interrupted attempt left off";
        let run_id = admit_container_run(&store, chat.id, task).await;
        let run_uuid = *run_id.as_uuid();

        // The sandbox already exists — a prior attempt provisioned it and
        // committed its handle before losing its own commit — so the backend
        // can only address it, never create it.
        let base_url = spawn_sandbox_agent().await;
        let backend = MockBackend::unreachable(base_url);
        let tag = SandboxTag::new();
        store
            .begin_sandbox_provision(
                run_uuid,
                &tag.to_string(),
                chrono::Utc::now() + chrono::Duration::seconds(600),
                tidebreak_core::SandboxAdmissionMode::AttachedOnly,
            )
            .await
            .unwrap();
        assert!(store
            .commit_sandbox_provision_handle(run_uuid, "prior-container")
            .await
            .unwrap());

        let provider = Arc::new(ScriptedProvider::new(vec![]));
        let runner = SandboxContainerRunner::new(
            store.clone(),
            backend.clone(),
            Arc::new(FixedResolver(provider)),
            fast_config(),
        );
        let outcome = runner
            .drive(run_id)
            .await
            .expect("driving succeeds")
            .expect("the container run is claimable");
        assert_eq!(outcome, SandboxContainerRunOutcome::Completed(run_id));
        // Reconciled, not re-provisioned — and its teardown still completed.
        assert_eq!(backend.provisions.load(Ordering::SeqCst), 0);
        assert_eq!(backend.destroys.load(Ordering::SeqCst), 1);
    })
    .await
    .expect("test completed within its time bound");
}

/// The recovery pass that replaces the lease reaper for container runs: a run
/// abandoned `running` under an expired lease is reclaimed under a fresh lease
/// on the SAME attempt, its committed container reattached and driven to the
/// result the reaper would have thrown away.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dead_drivers_container_run_is_recovered_to_completion() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let (_dir, store, chat) = store().await;
        let task = "finish what the dead driver started";
        let run_id = admit_container_run(&store, chat.id, task).await;
        let run_uuid = *run_id.as_uuid();

        // The dead driver: claimed the run, provisioned a container, committed
        // its handle — then vanished, leaving the lease to expire.
        let dead_token = Uuid::new_v4();
        store
            .claim_container_agent_run(run_id, dead_token, chrono::Duration::milliseconds(50), 4)
            .await
            .unwrap()
            .expect("the dead driver's claim succeeds");
        let base_url = spawn_sandbox_agent().await;
        let backend = MockBackend::unreachable(base_url);
        let tag = SandboxTag::new();
        store
            .begin_sandbox_provision(
                run_uuid,
                &tag.to_string(),
                chrono::Utc::now() + chrono::Duration::seconds(600),
                tidebreak_core::SandboxAdmissionMode::AttachedOnly,
            )
            .await
            .unwrap();
        assert!(store
            .commit_sandbox_provision_handle(run_uuid, "abandoned-container")
            .await
            .unwrap());
        tokio::time::sleep(Duration::from_millis(150)).await;

        let provider = Arc::new(ScriptedProvider::new(vec![]));
        let runner = SandboxContainerRunner::new(
            store.clone(),
            backend.clone(),
            Arc::new(FixedResolver(provider)),
            fast_config(),
        );
        let outcomes = runner.recover().await.expect("recovery succeeds");
        assert_eq!(
            outcomes,
            vec![SandboxContainerRunOutcome::Completed(run_id)]
        );

        let recovered = store.get_agent_run(run_id).await.unwrap().unwrap();
        assert_eq!(recovered.status, AgentRunStatus::Completed);
        assert_eq!(
            recovered.attempt_count, 1,
            "recovery re-drives the single attempt, never a second one"
        );
        // Reconciled the abandoned container: no new provision, torn down once.
        assert_eq!(backend.provisions.load(Ordering::SeqCst), 0);
        assert_eq!(backend.destroys.load(Ordering::SeqCst), 1);
    })
    .await
    .expect("test completed within its time bound");
}

/// A container run failed terminally through the fenced store path leaves its
/// provisioning record owing a teardown in the same transaction — the link the
/// deadline scan uses so an expired run's live container is swept, not leaked.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_terminal_container_failure_enqueues_its_teardown() {
    let (_dir, store, chat) = store().await;
    let run_id = admit_container_run(&store, chat.id, "fails terminally").await;
    let run_uuid = *run_id.as_uuid();
    let token = Uuid::new_v4();
    store
        .claim_container_agent_run(run_id, token, chrono::Duration::seconds(30), 4)
        .await
        .unwrap()
        .expect("the claim succeeds");
    let tag = SandboxTag::new();
    store
        .begin_sandbox_provision(
            run_uuid,
            &tag.to_string(),
            chrono::Utc::now() + chrono::Duration::seconds(600),
            tidebreak_core::SandboxAdmissionMode::AttachedOnly,
        )
        .await
        .unwrap();
    assert!(store
        .commit_sandbox_provision_handle(run_uuid, "doomed-container")
        .await
        .unwrap());

    store
        .fail_agent_run(
            run_id,
            token,
            "sandbox_agent_failed",
            "the loop ended without a result",
            chrono::Duration::seconds(1),
        )
        .await
        .unwrap()
        .expect("the terminal failure commits");

    let teardowns = store.list_sandbox_teardowns().await.unwrap();
    assert_eq!(teardowns.len(), 1);
    assert_eq!(teardowns[0].run_id, run_uuid);
    assert_eq!(teardowns[0].handle.as_deref(), Some("doomed-container"));
}

/// Container runs bypass the in-process scheduler's caps, so their own bound is
/// enforced at the claim: a second claim past the cap is refused and the run
/// stays queued, becoming claimable once a slot frees.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_container_claim_refuses_past_the_concurrency_cap() {
    let (_dir, store, chat) = store().await;
    // The cap is global across chats, so give each run its own chat (a chat
    // runs one turn at a time, and the admit helper claims the chat's turn).
    let other = Chat {
        id: ChatId::new(),
        title: Some("second container chat".into()),
        ..chat.clone()
    };
    store.create_chat(&other).await.unwrap();
    let first = admit_container_run(&store, chat.id, "occupies the only slot").await;
    let second = admit_container_run(&store, other.id, "waits for the slot").await;

    let first_token = Uuid::new_v4();
    store
        .claim_container_agent_run(first, first_token, chrono::Duration::seconds(30), 1)
        .await
        .unwrap()
        .expect("the first claim takes the only slot");
    assert!(
        store
            .claim_container_agent_run(second, Uuid::new_v4(), chrono::Duration::seconds(30), 1)
            .await
            .unwrap()
            .is_none(),
        "a claim past the cap must be refused, not queued into a second container"
    );
    // The refused run is still queued, not damaged: it claims once a slot frees.
    store
        .fail_agent_run(
            first,
            first_token,
            "sandbox_agent_failed",
            "released its slot",
            chrono::Duration::seconds(1),
        )
        .await
        .unwrap()
        .expect("the first run fails terminally");
    store
        .claim_container_agent_run(second, Uuid::new_v4(), chrono::Duration::seconds(30), 1)
        .await
        .unwrap()
        .expect("the freed slot admits the queued run");
}

/// A well-formed result that arrives after the run is already terminal fails
/// the fenced commit predicate and is retained as evidence instead — first
/// writer wins, and nothing is ever committed from it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_late_result_is_retained_as_evidence_not_committed() {
    let (_dir, store, chat) = store().await;
    let run_id = admit_container_run(&store, chat.id, "finishes too late").await;
    let run_uuid = *run_id.as_uuid();
    let token = Uuid::new_v4();
    store
        .claim_container_agent_run(run_id, token, chrono::Duration::seconds(30), 4)
        .await
        .unwrap()
        .expect("the claim succeeds");
    store
        .begin_sandbox_provision(
            run_uuid,
            &SandboxTag::new().to_string(),
            chrono::Utc::now() + chrono::Duration::seconds(600),
            tidebreak_core::SandboxAdmissionMode::AttachedOnly,
        )
        .await
        .unwrap();

    // The run goes terminal (the deadline scan, a cancellation) while the
    // container still works; its result then arrives and must not commit.
    store
        .fail_agent_run(
            run_id,
            token,
            "deadline_exceeded",
            "went terminal before the result arrived",
            chrono::Duration::seconds(1),
        )
        .await
        .unwrap()
        .expect("the terminal failure commits");
    assert!(store
        .submit_agent_run_result(run_id, token, "the late answer")
        .await
        .unwrap()
        .is_none());

    assert!(store
        .record_late_container_result_evidence(run_uuid, "the late answer")
        .await
        .unwrap());
    // A redelivery is a no-op, not an overwrite.
    assert!(!store
        .record_late_container_result_evidence(run_uuid, "a different answer")
        .await
        .unwrap());
    let record = store
        .get_sandbox_provision(run_uuid)
        .await
        .unwrap()
        .expect("the provisioning record exists");
    assert_eq!(
        record.late_result_evidence.as_deref(),
        Some("the late answer")
    );
    // The run's authoritative outcome is still the failure.
    let run = store.get_agent_run(run_id).await.unwrap().unwrap();
    assert_eq!(run.status, AgentRunStatus::Failed);
}

/// A result can win the in-memory select after cancellation already moved the
/// durable run to `cancelling`. The fenced result remains late evidence, while
/// the same driver must still acknowledge the exact cancellation instead of
/// abandoning the run in its nonterminal state.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_result_fenced_by_cancellation_still_finishes_cancellation() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (_dir, store, chat) = store().await;
        let run_id = admit_container_run(&store, chat.id, "result races cancellation").await;
        let lease_token = Uuid::new_v4();
        let run = store
            .claim_container_agent_run(run_id, lease_token, chrono::Duration::seconds(30), 4)
            .await
            .unwrap()
            .expect("the container run claims");
        store
            .begin_sandbox_provision(
                *run_id.as_uuid(),
                &SandboxTag::new().to_string(),
                chrono::Utc::now() + chrono::Duration::seconds(600),
                tidebreak_core::SandboxAdmissionMode::AttachedOnly,
            )
            .await
            .unwrap();

        let fault_store = TerminalFaultStore::new(store.clone());
        fault_store.block_next_result();
        let resolver: Arc<dyn ProviderResolver> =
            Arc::new(FixedResolver(Arc::new(ScriptedProvider::new(Vec::new()))));
        let runner = Arc::new(SandboxContainerRunner::new(
            fault_store.clone(),
            MockBackend::spawning(),
            resolver.clone(),
            fast_config(),
        ));
        let model_proxy = Arc::new(HostModelProxy {
            resolver,
            cancel: CancelToken::new(),
            lease_guard: None,
            config: AgentConfig::default(),
            spent: AtomicU32::new(0),
            budget: 24,
            accounting: None,
            observed: HostModelObservedAccounting::default(),
        });
        let finalize = tokio::spawn({
            let runner = runner.clone();
            let model_proxy = model_proxy.clone();
            async move {
                runner
                    .finish_after_quiescence(
                        &run,
                        lease_token,
                        &model_proxy,
                        &DriveEnd::Result("the raced answer".to_owned()),
                    )
                    .await
            }
        });

        fault_store.result_entered.notified().await;
        store
            .request_agent_run_cancellation(run_id)
            .await
            .unwrap()
            .expect("the cancellation request wins the durable race");
        fault_store.result_release.notify_one();

        let outcome = finalize.await.unwrap().expect("finalization succeeds");
        assert_eq!(outcome, SandboxContainerRunOutcome::Cancelled(run_id));
        let cancelled = store.get_agent_run(run_id).await.unwrap().unwrap();
        assert_eq!(cancelled.status, AgentRunStatus::Cancelled);
        let result = store
            .get_agent_run_result(run_id)
            .await
            .unwrap()
            .expect("cancellation writes its immutable receipt");
        assert_eq!(result.model_steps, 0);
        assert_eq!(result.usage, tidebreak_core::Usage::default());
        let provision = store
            .get_sandbox_provision(*run_id.as_uuid())
            .await
            .unwrap()
            .expect("the provisioning record exists");
        assert_eq!(
            provision.late_result_evidence.as_deref(),
            Some("the raced answer")
        );
    })
    .await
    .expect("test completed within its time bound");
}

/// Final accounting may remain unavailable past the execution lease that was
/// live when cancellation began. The exact durable cancellation identity keeps
/// renewing finalization authority until storage recovers, then records one
/// step and snapshots it into the immutable cancelled receipt.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancellation_finalization_survives_accounting_failure_past_execution_lease() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (_dir, store, chat) = store().await;
        let run_id = admit_container_run(&store, chat.id, "retry cancellation accounting").await;
        let lease_token = Uuid::new_v4();
        let lease = Duration::from_millis(150);
        let run = store
            .claim_container_agent_run(
                run_id,
                lease_token,
                chrono::Duration::from_std(lease).unwrap(),
                4,
            )
            .await
            .unwrap()
            .expect("the container run claims");
        store
            .request_agent_run_cancellation(run_id)
            .await
            .unwrap()
            .expect("the cancellation request lands");

        let usage = tidebreak_core::Usage {
            input_tokens: 19,
            output_tokens: 11,
            cache_read_input_tokens: 7,
            cache_creation_input_tokens: 3,
        };
        let fault_store = TerminalFaultStore::new(store.clone());
        fault_store.fail_accounting_until_released();
        let resolver: Arc<dyn ProviderResolver> =
            Arc::new(FixedResolver(Arc::new(ScriptedProvider::new(Vec::new()))));
        let runner = Arc::new(SandboxContainerRunner::new(
            fault_store.clone(),
            MockBackend::spawning(),
            resolver.clone(),
            SandboxContainerRunConfig {
                lease,
                heartbeat: Duration::from_millis(40),
                ..fast_config()
            },
        ));
        let model_proxy = Arc::new(HostModelProxy {
            resolver,
            cancel: CancelToken::new(),
            lease_guard: None,
            config: AgentConfig::default(),
            spent: AtomicU32::new(0),
            budget: 24,
            accounting: Some(HostModelAccounting {
                store: fault_store.clone(),
                run_id,
                lease_token,
                baseline: tokio::sync::Mutex::new((run.model_steps, run.usage)),
            }),
            observed: HostModelObservedAccounting::default(),
        });
        let mut observation = None;
        model_proxy
            .observed
            .add_usage(&mut observation, usage)
            .unwrap();

        let first_failure = fault_store.accounting_failure_observed.notified();
        tokio::pin!(first_failure);
        let original_expiry = run
            .lease_expires_at
            .expect("the claimed execution lease has an expiry");
        let finalize = tokio::spawn({
            let runner = runner.clone();
            let model_proxy = model_proxy.clone();
            async move {
                runner
                    .finish_after_quiescence(&run, lease_token, &model_proxy, &DriveEnd::LeaseLost)
                    .await
            }
        });

        tokio::time::timeout(Duration::from_secs(2), first_failure)
            .await
            .expect("final accounting reaches the injected outage");
        let until_original_expiry = original_expiry
            .signed_duration_since(chrono::Utc::now())
            .to_std()
            .unwrap_or_default();
        tokio::time::sleep(until_original_expiry + Duration::from_millis(75)).await;
        assert!(chrono::Utc::now() > original_expiry);
        let still_cancelling = store.get_agent_run(run_id).await.unwrap().unwrap();
        assert_eq!(still_cancelling.status, AgentRunStatus::Cancelling);
        assert!(still_cancelling
            .lease_expires_at
            .is_some_and(|expiry| expiry > chrono::Utc::now()));

        fault_store.release_accounting();
        let outcome = tokio::time::timeout(Duration::from_secs(2), finalize)
            .await
            .expect("finalization resumes after accounting storage recovers")
            .unwrap()
            .expect("accounting recovery finishes the exact cancellation");
        assert_eq!(outcome, SandboxContainerRunOutcome::Cancelled(run_id));
        assert!(fault_store.accounting_calls.load(Ordering::SeqCst) >= 2);

        let cancelled = store.get_agent_run(run_id).await.unwrap().unwrap();
        assert_eq!(cancelled.status, AgentRunStatus::Cancelled);
        assert_eq!(cancelled.model_steps, 1);
        assert_eq!(cancelled.usage, usage);
        let result = store
            .get_agent_run_result(run_id)
            .await
            .unwrap()
            .expect("cancellation writes its immutable receipt");
        assert_eq!(result.model_steps, 1);
        assert_eq!(result.usage, usage);
    })
    .await
    .expect("test completed within its time bound");
}

/// Cancelling a run mid-drive: the heartbeat refusal is read as cancellation,
/// the driver commits the terminal cancellation it still owns, and the teardown
/// that follows is what actually stops the container.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_attached_cancellation_is_acknowledged_and_torn_down() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let (_dir, store, chat) = store().await;
        let run_id = admit_container_run(&store, chat.id, "cancelled mid-flight").await;

        let backend = MockBackend::spawning();
        // Slow completions hold the drive open across several heartbeats, so
        // the cancellation lands while the container is genuinely working.
        let provider = Arc::new(ScriptedProvider::slow(vec![], Duration::from_secs(5)));
        let runner = Arc::new(SandboxContainerRunner::new(
            store.clone(),
            backend.clone(),
            Arc::new(FixedResolver(provider)),
            SandboxContainerRunConfig {
                lease: Duration::from_secs(2),
                heartbeat: Duration::from_millis(100),
                ..fast_config()
            },
        ));
        let drive = tokio::spawn({
            let runner = runner.clone();
            async move { runner.drive(run_id).await }
        });
        // Let the drive claim and attach, then cancel out from under it.
        tokio::time::sleep(Duration::from_millis(500)).await;
        store
            .request_agent_run_cancellation(run_id)
            .await
            .unwrap()
            .expect("the cancellation request lands");

        let outcome = drive
            .await
            .unwrap()
            .expect("driving succeeds")
            .expect("the container run is claimable");
        assert_eq!(outcome, SandboxContainerRunOutcome::Cancelled(run_id));
        let cancelled = store.get_agent_run(run_id).await.unwrap().unwrap();
        assert_eq!(cancelled.status, AgentRunStatus::Cancelled);
        // The container did not outlive the cancellation.
        assert_eq!(backend.destroys.load(Ordering::SeqCst), 1);
    })
    .await
    .expect("test completed within its time bound");
}

/// Cancellation interrupts a backend create that has not returned a handle,
/// rather than waiting for the next heartbeat or for provisioning to finish.
/// The pre-create durable tag moves to teardown with the cancelled run, so a
/// backend side effect that escaped future cancellation is reclaimed by the
/// orphan sweep and the run is never provisioned a second time.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancellation_interrupts_held_provision_and_sweeps_its_tagged_side_effect() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (_dir, store, chat) = store().await;
        let run_id = admit_container_run(&store, chat.id, "cancel while provisioning").await;
        let backend = HeldProvisionBackend::new();
        let steering = Arc::new(SandboxSteerGuard::default());
        let runner = Arc::new(
            SandboxContainerRunner::new(
                store.clone(),
                backend.clone(),
                Arc::new(FixedResolver(Arc::new(ScriptedProvider::new(vec![])))),
                SandboxContainerRunConfig {
                    lease: Duration::from_secs(60),
                    heartbeat: Duration::from_secs(30),
                    ..fast_config()
                },
            )
            .with_steering(steering.clone()),
        );
        let drive = tokio::spawn({
            let runner = runner.clone();
            async move { runner.drive(run_id).await }
        });

        backend.started.notified().await;
        let intended = store
            .get_sandbox_provision(*run_id.as_uuid())
            .await
            .unwrap()
            .expect("the provisioning intent precedes the held backend call");
        assert_eq!(
            intended.state,
            tidebreak_core::SandboxProvisionState::Intended
        );
        let tag = intended.tag.parse::<SandboxTag>().unwrap();

        store
            .request_agent_run_cancellation(run_id)
            .await
            .unwrap()
            .expect("the cancellation request lands");
        let signal = store
            .get_agent_run_cancellation_signal(run_id)
            .await
            .unwrap()
            .expect("the cancellation receipt names the exact drive");
        assert!(
            steering.cancel_container_drive(run_id, signal.lease_token),
            "the held provisioning call is registered under the exact durable lease"
        );

        let outcome = tokio::time::timeout(Duration::from_secs(1), drive)
            .await
            .expect("cancellation must not wait for provision or the 30-second heartbeat")
            .unwrap()
            .expect("driving succeeds")
            .expect("the container run is claimable");
        assert_eq!(outcome, SandboxContainerRunOutcome::Cancelled(run_id));
        assert!(
            backend.dropped.load(Ordering::SeqCst),
            "the losing provisioning future is dropped before cancellation returns"
        );
        assert!(
            !backend.returned.load(Ordering::SeqCst),
            "the run cancels before the backend is released"
        );
        assert_eq!(backend.provisions.load(Ordering::SeqCst), 1);

        let cancelled = store.get_agent_run(run_id).await.unwrap().unwrap();
        assert_eq!(cancelled.status, AgentRunStatus::Cancelled);
        let teardown = store
            .get_sandbox_provision(*run_id.as_uuid())
            .await
            .unwrap()
            .expect("the ambiguous create remains durably correlated");
        assert_eq!(
            teardown.state,
            tidebreak_core::SandboxProvisionState::Teardown
        );
        assert_eq!(teardown.handle, None);
        assert!(store.live_sandbox_tags().await.unwrap().is_empty());

        // A terminal run cannot start another create, even while the first
        // backend side effect is known only by its tag.
        assert!(runner.drive(run_id).await.unwrap().is_none());
        assert_eq!(backend.provisions.load(Ordering::SeqCst), 1);

        runner.sweep().await.expect("the tag sweep succeeds");
        assert_eq!(backend.reclaimed.lock().unwrap().as_slice(), &[tag]);
        assert!(backend.orphans.lock().unwrap().is_empty());
        assert!(store.list_sandbox_teardowns().await.unwrap().is_empty());
        assert_eq!(
            store
                .get_sandbox_provision(*run_id.as_uuid())
                .await
                .unwrap()
                .unwrap()
                .state,
            tidebreak_core::SandboxProvisionState::Done
        );
    })
    .await
    .expect("test completed within its time bound");
}

/// A cancellation committed through another server process cannot use this
/// process's exact-drive registry. The short durable fence must still drop a
/// held create promptly, terminalize the immutable cancellation, and leave the
/// ambiguous tagged side effect exclusively to teardown/orphan recovery.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cross_process_cancellation_interrupts_held_provision_without_a_local_signal() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (_dir, store, chat) = store().await;
        let run_id = admit_container_run(&store, chat.id, "remote cancel while provisioning").await;
        let backend = HeldProvisionBackend::new();
        let provider = Arc::new(ScriptedProvider::new(vec!["must not execute".to_owned()]));
        let drive_guard = Arc::new(SandboxSteerGuard::default());
        let remote_guard = SandboxSteerGuard::default();
        let runner = Arc::new(
            SandboxContainerRunner::new(
                store.clone(),
                backend.clone(),
                Arc::new(FixedResolver(provider.clone())),
                SandboxContainerRunConfig {
                    lease: Duration::from_secs(60),
                    heartbeat: Duration::from_secs(30),
                    durable_fence_interval: Duration::from_millis(10),
                    ..fast_config()
                },
            )
            .with_steering(drive_guard),
        );
        let drive = tokio::spawn({
            let runner = runner.clone();
            async move { runner.drive(run_id).await }
        });

        backend.started.notified().await;
        let intended = store
            .get_sandbox_provision(*run_id.as_uuid())
            .await
            .unwrap()
            .expect("the exact durable admission writes its tag before create");
        let tag = intended.tag.parse::<SandboxTag>().unwrap();

        store
            .request_agent_run_cancellation(run_id)
            .await
            .unwrap()
            .expect("the remote cancellation commits");
        let signal = store
            .get_agent_run_cancellation_signal(run_id)
            .await
            .unwrap()
            .expect("the immutable cancellation receipt exists");
        assert!(
            !remote_guard.cancel_container_drive(run_id, signal.lease_token),
            "the cancelling process owns no registration for the remote drive"
        );

        let outcome = tokio::time::timeout(Duration::from_secs(1), drive)
            .await
            .expect("the durable watcher must beat the 30-second heartbeat")
            .unwrap()
            .expect("driving succeeds")
            .expect("the run was claimed");
        assert_eq!(outcome, SandboxContainerRunOutcome::Cancelled(run_id));
        assert!(backend.dropped.load(Ordering::SeqCst));
        assert!(!backend.returned.load(Ordering::SeqCst));
        assert_eq!(backend.provisions.load(Ordering::SeqCst), 1);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);

        let record = store
            .get_sandbox_provision(*run_id.as_uuid())
            .await
            .unwrap()
            .expect("the ambiguous create remains correlated");
        assert_eq!(
            record.state,
            tidebreak_core::SandboxProvisionState::Teardown
        );
        assert_eq!(record.handle, None);
        runner.sweep().await.expect("the tag sweep succeeds");
        assert_eq!(backend.reclaimed.lock().unwrap().as_slice(), &[tag]);
        assert!(backend.orphans.lock().unwrap().is_empty());
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    })
    .await
    .expect("test completed within its time bound");
}

/// Model-policy resolution may block in storage after the exact drive is
/// registered. A cancellation committed in another process is observed by the
/// durable watcher, which drops that setup future and remains the cancellation
/// finalizer without provisioning or reaching a provider.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cross_process_cancellation_interrupts_blocked_model_resolution() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (_dir, store, chat) = store().await;
        let run_id =
            admit_container_run(&store, chat.id, "remote cancel during model lookup").await;
        let fault_store =
            TerminalFaultStore::with_setup_fault(store.clone(), SetupFault::ModelResolution);
        let backend = MockBackend::spawning();
        let provider = Arc::new(ScriptedProvider::new(vec!["must not execute".to_owned()]));
        let runner = Arc::new(SandboxContainerRunner::new(
            fault_store.clone(),
            backend.clone(),
            Arc::new(FixedResolver(provider.clone())),
            SandboxContainerRunConfig {
                lease: Duration::from_secs(60),
                heartbeat: Duration::from_secs(30),
                durable_fence_interval: Duration::from_millis(10),
                ..fast_config()
            },
        ));
        let drive = tokio::spawn({
            let runner = runner.clone();
            async move { runner.drive(run_id).await }
        });

        fault_store.setup_entered.notified().await;
        store
            .request_agent_run_cancellation(run_id)
            .await
            .unwrap()
            .expect("the remote cancellation commits");

        let outcome = tokio::time::timeout(Duration::from_secs(1), drive)
            .await
            .expect("blocked model resolution is preempted by durable cancellation")
            .unwrap()
            .expect("driving succeeds")
            .expect("the run was claimed");
        assert_eq!(outcome, SandboxContainerRunOutcome::Cancelled(run_id));
        assert_eq!(backend.provisions.load(Ordering::SeqCst), 0);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        assert!(store
            .get_sandbox_provision(*run_id.as_uuid())
            .await
            .unwrap()
            .is_none());
    })
    .await
    .expect("test completed within its time bound");
}

/// A provisioning transaction may yield after taking the shared agent-run
/// claim lock. Once the short durable fence starts waiting for that lock, the
/// runner must keep polling the setup future so it can finish and release the
/// lock; awaiting the heartbeat inline self-deadlocks this exact interleaving.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pre_attach_fence_keeps_polling_setup_that_holds_the_claim_lock() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (_dir, store, chat) = store().await;
        let run_id = admit_container_run(&store, chat.id, "yield while holding claim lock").await;
        let lease_token = Uuid::new_v4();
        let fault_store = TerminalFaultStore::with_setup_fault(
            store.clone(),
            SetupFault::ProvisionIntentClaimLockYield,
        );
        let run = fault_store
            .claim_container_agent_run(run_id, lease_token, chrono::Duration::seconds(60), 4)
            .await
            .unwrap()
            .expect("the fixture run claims");
        let backend = MockBackend::spawning();
        let provider = Arc::new(ScriptedProvider::new(vec![
            "completed after fence".to_owned()
        ]));
        let runner = Arc::new(SandboxContainerRunner::new(
            fault_store.clone(),
            backend.clone(),
            Arc::new(FixedResolver(provider.clone())),
            SandboxContainerRunConfig {
                lease: Duration::from_secs(60),
                heartbeat: Duration::from_secs(30),
                durable_fence_interval: Duration::from_millis(10),
                ..fast_config()
            },
        ));
        let wait = tokio::spawn({
            let runner = runner.clone();
            let fault_store = fault_store.clone();
            async move {
                let cancel = CancelToken::new();
                let tag = SandboxTag::new().to_string();
                let window_expires_at = chrono::Utc::now() + chrono::Duration::seconds(60);
                runner
                    .await_pre_attach(
                        &run,
                        lease_token,
                        &cancel,
                        fault_store.begin_sandbox_provision_for_agent_run(
                            run_id,
                            lease_token,
                            &tag,
                            window_expires_at,
                            tidebreak_core::SandboxAdmissionMode::AttachedOnly,
                        ),
                    )
                    .await
            }
        });

        fault_store.setup_entered.notified().await;
        fault_store.fence_entered.notified().await;
        fault_store.setup_release.notify_one();

        let outcome = tokio::time::timeout(Duration::from_secs(5), wait)
            .await
            .expect("the setup and contending durable fence must both keep making progress")
            .unwrap();
        assert!(matches!(
            outcome,
            PreAttachEnd::Completed(Ok(Some(
                tidebreak_core::BeginSandboxProvisionOutcome::Started
            )))
        ));
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        assert_eq!(backend.provisions.load(Ordering::SeqCst), 0);
        assert!(store
            .get_sandbox_provision(*run_id.as_uuid())
            .await
            .unwrap()
            .is_some());
    })
    .await
    .expect("test completed within its time bound");
}

/// The provisioning-intent transaction itself is the exact durable admission
/// fence. If remote cancellation gets the shared claim lock first, the delayed
/// transaction returns no authority and no external create begins.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn exact_provision_admission_refuses_a_remotely_cancelled_claim() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (_dir, store, chat) = store().await;
        let run_id =
            admit_container_run(&store, chat.id, "cancel before exact provision fence").await;
        let fault_store =
            TerminalFaultStore::with_setup_fault(store.clone(), SetupFault::ProvisionIntentDelayed);
        let backend = MockBackend::spawning();
        let provider = Arc::new(ScriptedProvider::new(vec!["must not execute".to_owned()]));
        let runner = Arc::new(SandboxContainerRunner::new(
            fault_store.clone(),
            backend.clone(),
            Arc::new(FixedResolver(provider.clone())),
            SandboxContainerRunConfig {
                lease: Duration::from_secs(60),
                heartbeat: Duration::from_secs(30),
                durable_fence_interval: Duration::from_secs(30),
                ..fast_config()
            },
        ));
        let drive = tokio::spawn({
            let runner = runner.clone();
            async move { runner.drive(run_id).await }
        });

        fault_store.setup_entered.notified().await;
        store
            .request_agent_run_cancellation(run_id)
            .await
            .unwrap()
            .expect("the remote cancellation commits before admission resumes");
        fault_store.setup_release.notify_one();

        let outcome = tokio::time::timeout(Duration::from_secs(1), drive)
            .await
            .expect("the refused exact admission finalizes cancellation promptly")
            .unwrap()
            .expect("driving succeeds")
            .expect("the run was claimed");
        assert_eq!(outcome, SandboxContainerRunOutcome::Cancelled(run_id));
        assert_eq!(backend.provisions.load(Ordering::SeqCst), 0);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        assert!(store
            .get_sandbox_provision(*run_id.as_uuid())
            .await
            .unwrap()
            .is_none());
    })
    .await
    .expect("test completed within its time bound");
}

/// A provisioning-intent storage error can arrive after remote cancellation
/// committed. The driver must reconcile the immutable receipt before returning
/// that setup error, keeping the only finalizer alive and never calling the
/// backend or provider.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failing_provision_intent_reconciles_remote_cancellation() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (_dir, store, chat) = store().await;
        let run_id =
            admit_container_run(&store, chat.id, "cancel races provision persistence").await;
        let fault_store =
            TerminalFaultStore::with_setup_fault(store.clone(), SetupFault::ProvisionIntentFailure);
        let backend = MockBackend::spawning();
        let provider = Arc::new(ScriptedProvider::new(vec!["must not execute".to_owned()]));
        let runner = Arc::new(SandboxContainerRunner::new(
            fault_store.clone(),
            backend.clone(),
            Arc::new(FixedResolver(provider.clone())),
            SandboxContainerRunConfig {
                lease: Duration::from_secs(60),
                heartbeat: Duration::from_secs(30),
                durable_fence_interval: Duration::from_secs(30),
                ..fast_config()
            },
        ));
        let drive = tokio::spawn({
            let runner = runner.clone();
            async move { runner.drive(run_id).await }
        });

        fault_store.setup_entered.notified().await;
        store
            .request_agent_run_cancellation(run_id)
            .await
            .unwrap()
            .expect("the remote cancellation commits");
        fault_store.setup_release.notify_one();

        let outcome = tokio::time::timeout(Duration::from_secs(1), drive)
            .await
            .expect("the setup error is reconciled as cancellation")
            .unwrap()
            .expect("driving succeeds")
            .expect("the run was claimed");
        assert_eq!(outcome, SandboxContainerRunOutcome::Cancelled(run_id));
        assert_eq!(backend.provisions.load(Ordering::SeqCst), 0);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        assert!(store
            .get_sandbox_provision(*run_id.as_uuid())
            .await
            .unwrap()
            .is_none());
    })
    .await
    .expect("test completed within its time bound");
}

/// A heartbeat reports whether it extended the lease, not whether the exact
/// claim is still valid. When the requested lease is longer than the run's
/// remaining absolute lifetime, the claim is clamped to the deadline and every
/// renewal is a deterministic no-op. The driver must validate that live claim
/// instead of treating the no-op as cancellation or lease loss.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_noop_heartbeat_does_not_revoke_a_live_container_drive() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (_dir, store, chat) = store().await;
        let run_id = admit_container_run(&store, chat.id, "finish under a clamped lease").await;
        let lease_token = Uuid::new_v4();
        let lease = chrono::Duration::hours(2);
        let run = store
            .claim_container_agent_run(run_id, lease_token, lease, 4)
            .await
            .unwrap()
            .expect("the fixture run claims");

        assert!(
            store
                .validate_agent_run_execution(
                    run_id,
                    lease_token,
                    AgentRunExecutionLocation::Container,
                )
                .await
                .unwrap()
        );
        assert!(
            !store
                .heartbeat_agent_run(run_id, lease_token, lease)
                .await
                .unwrap(),
            "the deadline-clamped lease makes extension a deterministic no-op"
        );
        assert!(
            store
                .validate_agent_run_execution(
                    run_id,
                    lease_token,
                    AgentRunExecutionLocation::Container,
                )
                .await
                .unwrap(),
            "the exact execution claim remains live after the no-op"
        );

        let backend = MockBackend::spawning();
        let provider = Arc::new(ScriptedProvider::slow(
            vec!["finished under the live claim".to_owned()],
            Duration::from_millis(100),
        ));
        let runner = SandboxContainerRunner::new(
            store.clone(),
            backend.clone(),
            Arc::new(FixedResolver(provider.clone())),
            SandboxContainerRunConfig {
                lease: Duration::from_secs(2 * 60 * 60),
                heartbeat: Duration::from_millis(10),
                durable_fence_interval: Duration::from_millis(10),
                ..fast_config()
            },
        );

        let outcome = runner
            .drive_claimed(run, lease_token)
            .await
            .expect("driving succeeds despite no-op renewals");
        assert_eq!(outcome, SandboxContainerRunOutcome::Completed(run_id));
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        assert_eq!(backend.provisions.load(Ordering::SeqCst), 1);
        assert_eq!(backend.destroys.load(Ordering::SeqCst), 1);
    })
    .await
    .expect("test completed within its time bound");
}

/// Cancellation waits for an in-flight reverse model inference to quiesce,
/// persists the usage it already observed, and only then snapshots the
/// immutable cancelled result.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn attached_cancellation_accounts_usage_observed_before_reverse_stream_drop() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let (_dir, store, chat) = store().await;
        let run_id = admit_container_run(&store, chat.id, "account before cancellation").await;

        let backend = MockBackend::spawning();
        let stalled = Arc::new(Notify::new());
        let dropped = Arc::new(AtomicBool::new(false));
        let usage = tidebreak_core::Usage {
            input_tokens: 11,
            output_tokens: 7,
            cache_read_input_tokens: 5,
            cache_creation_input_tokens: 3,
        };
        let provider = Arc::new(UsageThenPendingProvider {
            stalled: stalled.clone(),
            dropped: dropped.clone(),
            drop_observed: Arc::new(Notify::new()),
            drop_gate: None,
            calls: AtomicUsize::new(0),
            usage,
        });
        let runner = Arc::new(SandboxContainerRunner::new(
            store.clone(),
            backend.clone(),
            Arc::new(UsageThenPendingResolver(provider)),
            SandboxContainerRunConfig {
                lease: Duration::from_secs(2),
                heartbeat: Duration::from_millis(100),
                ..fast_config()
            },
        ));
        let drive = tokio::spawn({
            let runner = runner.clone();
            async move { runner.drive(run_id).await }
        });

        stalled.notified().await;
        store
            .request_agent_run_cancellation(run_id)
            .await
            .unwrap()
            .expect("the cancellation request lands");

        let outcome = drive
            .await
            .unwrap()
            .expect("driving succeeds")
            .expect("the container run is claimable");
        assert_eq!(outcome, SandboxContainerRunOutcome::Cancelled(run_id));
        assert!(
            dropped.load(Ordering::SeqCst),
            "the reverse provider stream must quiesce before cancellation completes"
        );

        let cancelled = store.get_agent_run(run_id).await.unwrap().unwrap();
        assert_eq!(cancelled.status, AgentRunStatus::Cancelled);
        assert_eq!(cancelled.model_steps, 1);
        assert_eq!(cancelled.usage, usage);

        let result = store
            .get_agent_run_result(run_id)
            .await
            .unwrap()
            .expect("cancellation persists an immutable result");
        assert_eq!(result.model_steps, 1);
        assert_eq!(result.usage, usage);
        assert_eq!(backend.destroys.load(Ordering::SeqCst), 1);
    })
    .await
    .expect("test completed within its time bound");
}

/// Cancellation closes the active connection and the host's admission gate
/// immediately after the durable route transition, without waiting for the
/// next lease heartbeat. A reverse request attempted after that signal may see
/// either the stable cancellation refusal or connection teardown, but it must
/// never start another provider call.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn committed_cancellation_wakes_container_and_fences_reverse_egress_before_heartbeat() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (_dir, store, chat) = store().await;
        let run_id = admit_container_run(&store, chat.id, "cancel before heartbeat").await;

        let sandbox = SandboxRun::new(
            [Capability::ModelInference],
            Some(TransportSecret::new("test-secret")),
        );
        let base_url = spawn_sandbox_run(sandbox.clone()).await;
        let backend = MockBackend::unreachable(base_url);
        let provider = Arc::new(ScriptedProvider::new(vec![
            "first answer".to_owned(),
            "must not execute".to_owned(),
        ]));
        let steering = Arc::new(SandboxSteerGuard::default());
        let runner = Arc::new(
            SandboxContainerRunner::new(
                store.clone(),
                backend.clone(),
                Arc::new(FixedResolver(provider.clone())),
                SandboxContainerRunConfig {
                    lease: Duration::from_secs(60),
                    // Attachment below proves the registration/revalidation
                    // heartbeat completed. The cancellation must finish long
                    // before this next periodic heartbeat can fire.
                    heartbeat: Duration::from_secs(30),
                    ..fast_config()
                },
            )
            .with_steering(steering.clone()),
        );
        let drive = tokio::spawn({
            let runner = runner.clone();
            async move { runner.drive(run_id).await }
        });

        let _init = sandbox.init().await;
        let first = sandbox
            .call(
                OperationId::new(),
                ReverseRequest::ModelInference(ModelInferenceParams {
                    prompt: "first reverse request".to_owned(),
                }),
            )
            .await;
        assert!(matches!(
            first,
            ReverseOutcome::Settled(Response::Ok(ReverseResult::ModelInference(_)))
        ));
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);

        store
            .request_agent_run_cancellation(run_id)
            .await
            .unwrap()
            .expect("the cancellation request lands");
        let signal = store
            .get_agent_run_cancellation_signal(run_id)
            .await
            .unwrap()
            .expect("the durable cancellation receipt names the exact drive");
        assert!(
            steering.cancel_container_drive(run_id, signal.lease_token),
            "the attached container drive is registered under the durable lease"
        );

        let late = tokio::time::timeout(
            Duration::from_secs(1),
            sandbox.call(
                OperationId::new(),
                ReverseRequest::ModelInference(ModelInferenceParams {
                    prompt: "post-cancellation reverse request".to_owned(),
                }),
            ),
        )
        .await
        .expect("the post-cancellation reverse request settles or disconnects promptly");
        match late {
            ReverseOutcome::Disconnected => {}
            ReverseOutcome::Settled(Response::Error(error)) => {
                assert_eq!(error.code, ErrorCode::Cancelled);
            }
            other => panic!("post-cancellation reverse request returned {other:?}"),
        }

        let outcome = tokio::time::timeout(Duration::from_secs(2), drive)
            .await
            .expect("cancellation must not wait for the 30-second heartbeat")
            .unwrap()
            .expect("driving succeeds")
            .expect("the container run is claimable");
        assert_eq!(outcome, SandboxContainerRunOutcome::Cancelled(run_id));
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            1,
            "no provider call may begin after the committed cancellation signal"
        );
        assert_eq!(
            store.get_agent_run(run_id).await.unwrap().unwrap().status,
            AgentRunStatus::Cancelled
        );
        assert_eq!(backend.destroys.load(Ordering::SeqCst), 1);
    })
    .await
    .expect("test completed within its time bound");
}

/// Cancellation closes the active connection and the host's admission gate
/// before it snapshots terminal totals. A fresh reverse request attempted while
/// the first provider stream is being cancelled must never reach the provider or
/// keep the cancellation waiting forever.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancellation_refuses_a_reverse_request_attempted_during_quiescence() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let (_dir, store, chat) = store().await;
        let run_id = admit_container_run(&store, chat.id, "close admission on cancellation").await;

        let sandbox = SandboxRun::new(
            [Capability::ModelInference],
            Some(TransportSecret::new("test-secret")),
        );
        let base_url = spawn_sandbox_run(sandbox.clone()).await;
        let backend = MockBackend::unreachable(base_url);
        let stalled = Arc::new(Notify::new());
        let dropped = Arc::new(AtomicBool::new(false));
        let drop_observed = Arc::new(Notify::new());
        let drop_gate = Arc::new(Barrier::new(2));
        let usage = tidebreak_core::Usage {
            input_tokens: 13,
            output_tokens: 8,
            cache_read_input_tokens: 5,
            cache_creation_input_tokens: 2,
        };
        let provider = Arc::new(UsageThenPendingProvider {
            stalled: stalled.clone(),
            dropped: dropped.clone(),
            drop_observed: drop_observed.clone(),
            drop_gate: Some(drop_gate.clone()),
            calls: AtomicUsize::new(0),
            usage,
        });
        let steering = Arc::new(SandboxSteerGuard::default());
        let runner = Arc::new(
            SandboxContainerRunner::new(
                store.clone(),
                backend.clone(),
                Arc::new(UsageThenPendingResolver(provider.clone())),
                SandboxContainerRunConfig {
                    lease: Duration::from_secs(2),
                    heartbeat: Duration::from_millis(50),
                    ..fast_config()
                },
            )
            .with_steering(steering.clone()),
        );
        let drive = tokio::spawn({
            let runner = runner.clone();
            async move { runner.drive(run_id).await }
        });

        let _init = sandbox.init().await;
        let first_call = tokio::spawn({
            let sandbox = sandbox.clone();
            async move {
                sandbox
                    .call(
                        OperationId::new(),
                        ReverseRequest::ModelInference(ModelInferenceParams {
                            prompt: "first reverse request".to_owned(),
                        }),
                    )
                    .await
            }
        });
        stalled.notified().await;

        let first_drop = drop_observed.notified();
        tokio::pin!(first_drop);
        store
            .request_agent_run_cancellation(run_id)
            .await
            .unwrap()
            .expect("the cancellation request lands");
        let signal = store
            .get_agent_run_cancellation_signal(run_id)
            .await
            .unwrap()
            .expect("the durable cancellation receipt names the exact drive");
        assert!(steering.cancel_container_drive(run_id, signal.lease_token));
        first_drop.await;

        // The first stream is now blocked in Drop, holding quiescence open.
        // Attempt a fresh request during that exact window. Before the fix the
        // still-live connection admitted it after `cancel_all` took its snapshot,
        // spending a second provider call and wedging `wait_idle` forever.
        let late_call = tokio::spawn({
            let sandbox = sandbox.clone();
            async move {
                sandbox
                    .call(
                        OperationId::new(),
                        ReverseRequest::ModelInference(ModelInferenceParams {
                            prompt: "late reverse request".to_owned(),
                        }),
                    )
                    .await
            }
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        tokio::task::spawn_blocking(move || drop_gate.wait())
            .await
            .unwrap();

        let outcome = tokio::time::timeout(Duration::from_secs(5), drive)
            .await
            .expect("cancellation must not hang behind a late reverse request")
            .unwrap()
            .expect("driving succeeds")
            .expect("the container run is claimable");
        assert_eq!(outcome, SandboxContainerRunOutcome::Cancelled(run_id));
        assert!(dropped.load(Ordering::SeqCst));
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            1,
            "the request attempted after terminal cleanup began must not execute"
        );

        let first = tokio::time::timeout(Duration::from_secs(1), first_call)
            .await
            .expect("the original sandbox call observes connection teardown")
            .unwrap();
        assert!(matches!(first, ReverseOutcome::Disconnected));
        late_call.abort();
        let _ = late_call.await;

        let cancelled = store.get_agent_run(run_id).await.unwrap().unwrap();
        assert_eq!(cancelled.status, AgentRunStatus::Cancelled);
        assert_eq!(cancelled.model_steps, 1);
        assert_eq!(cancelled.usage, usage);
        let result = store
            .get_agent_run_result(run_id)
            .await
            .unwrap()
            .expect("cancellation persists an immutable result");
        assert_eq!(result.model_steps, 1);
        assert_eq!(result.usage, usage);
        assert_eq!(backend.destroys.load(Ordering::SeqCst), 1);
    })
    .await
    .expect("test completed within its time bound");
}

/// A terminal event can arrive while a detached reverse responder is still
/// pending. The driver must close and quiesce that responder, persist its
/// observed usage, and only then commit the result that snapshots the totals.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn terminal_result_waits_for_pending_reverse_accounting() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let (_dir, store, chat) = store().await;
        let run_id = admit_container_run(&store, chat.id, "result races reverse inference").await;

        let sandbox = SandboxRun::new(
            [Capability::ModelInference],
            Some(TransportSecret::new("test-secret")),
        );
        let base_url = spawn_sandbox_run(sandbox.clone()).await;
        let backend = MockBackend::unreachable(base_url);
        let stalled = Arc::new(Notify::new());
        let dropped = Arc::new(AtomicBool::new(false));
        let usage = tidebreak_core::Usage {
            input_tokens: 17,
            output_tokens: 9,
            cache_read_input_tokens: 4,
            cache_creation_input_tokens: 1,
        };
        let provider = Arc::new(UsageThenPendingProvider {
            stalled: stalled.clone(),
            dropped: dropped.clone(),
            drop_observed: Arc::new(Notify::new()),
            drop_gate: None,
            calls: AtomicUsize::new(0),
            usage,
        });
        let runner = Arc::new(SandboxContainerRunner::new(
            store.clone(),
            backend.clone(),
            Arc::new(UsageThenPendingResolver(provider.clone())),
            fast_config(),
        ));
        let drive = tokio::spawn({
            let runner = runner.clone();
            async move { runner.drive(run_id).await }
        });

        let _init = sandbox.init().await;
        let reverse_call = tokio::spawn({
            let sandbox = sandbox.clone();
            async move {
                sandbox
                    .call(
                        OperationId::new(),
                        ReverseRequest::ModelInference(ModelInferenceParams {
                            prompt: "pending reverse request".to_owned(),
                        }),
                    )
                    .await
            }
        });
        stalled.notified().await;
        sandbox
            .emit_result("terminal answer")
            .await
            .expect("the sandbox emits its terminal result");

        let outcome = tokio::time::timeout(Duration::from_secs(5), drive)
            .await
            .expect("terminal result must not race past reverse quiescence")
            .unwrap()
            .expect("driving succeeds")
            .expect("the container run is claimable");
        assert_eq!(outcome, SandboxContainerRunOutcome::Completed(run_id));
        assert!(
            dropped.load(Ordering::SeqCst),
            "the pending provider stream is dropped before the result commits"
        );
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);

        let reverse = tokio::time::timeout(Duration::from_secs(1), reverse_call)
            .await
            .expect("the sandbox call observes connection teardown")
            .unwrap();
        assert!(matches!(reverse, ReverseOutcome::Disconnected));

        let completed = store.get_agent_run(run_id).await.unwrap().unwrap();
        assert_eq!(completed.status, AgentRunStatus::Completed);
        assert_eq!(completed.model_steps, 1);
        assert_eq!(completed.usage, usage);
        let result = store
            .get_agent_run_result(run_id)
            .await
            .unwrap()
            .expect("completion persists an immutable result");
        assert_eq!(result.model_steps, 1);
        assert_eq!(result.usage, usage);
        assert_eq!(backend.destroys.load(Ordering::SeqCst), 1);
    })
    .await
    .expect("test completed within its time bound");
}

/// Steering a live sandbox-resident run: an instruction the host sends while the
/// container is mid-run reaches the agent's *next* model step, and a run nobody
/// is attached to refuses the instruction instead of queueing it.
///
/// This is the whole contract of the feature across the real stack — the host
/// API, the wire frame over a loopback socket, and the in-container loop folding
/// the text into its transcript — so it is driven end to end rather than
/// asserted on the frame in isolation.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn steering_a_live_container_run_reaches_the_agents_next_model_step() {
    // This test starts a real loopback sandbox, a host-side wire driver, and a
    // nested multi-thread runtime. Under the full server suite those tasks can
    // spend most of the ordinary 30-second guard waiting behind the other
    // integration-style tests even though the exercised flow takes well under
    // a second in isolation. Keep a hard bound, but leave enough headroom for
    // the parallel suite to schedule the end-to-end stack.
    tokio::time::timeout(Duration::from_secs(60), async {
        const INSTRUCTION: &str = "stop listing files and report what you have";
        let (_dir, store, chat) = store().await;
        let run_id = admit_container_run(&store, chat.id, "inspect the workspace").await;

        let backend = MockBackend::spawning();
        let gate = Arc::new(StepGate::default());
        // Two tool steps then a final answer, so the run is still working when
        // the instruction arrives and has a later step to apply it on.
        let provider = Arc::new(ScriptedProvider::gated(
            vec![
                "use-tool:list_dir:{\"path\":\".\"}".to_owned(),
                "use-tool:list_dir:{\"path\":\".\"}".to_owned(),
                "nothing but an empty workspace".to_owned(),
            ],
            gate.clone(),
        ));
        let steering = Arc::new(SandboxSteerGuard::default());
        let runner = Arc::new(
            SandboxContainerRunner::new(
                store.clone(),
                backend.clone(),
                Arc::new(FixedResolver(provider.clone())),
                fast_config(),
            )
            .with_steering(steering.clone()),
        );

        // Nothing is attached yet: the instruction is refused outright rather
        // than parked for a connection that may never exist.
        assert_eq!(
            steering.steer(run_id, "too early to steer".to_owned()),
            Err(SandboxSteerRefusal::NotAttached),
        );

        let drive = tokio::spawn({
            let runner = runner.clone();
            async move { runner.drive(run_id).await }
        });

        // The first model step proves the container is attached and working, so
        // the instruction below is genuinely mid-run.
        gate.started.notified().await;
        steering
            .steer(run_id, INSTRUCTION.to_owned())
            .expect("a live attached run accepts steering");
        // Let the frame cross the socket while the sandbox is parked on its
        // model call, then release the step it was waiting on.
        tokio::time::sleep(Duration::from_millis(200)).await;
        gate.release.notify_one();

        let outcome = drive
            .await
            .unwrap()
            .expect("driving succeeds")
            .expect("the container run is claimable");
        assert_eq!(outcome, SandboxContainerRunOutcome::Completed(run_id));

        // The sandbox folded the instruction into a later step's transcript: the
        // host proxied a prompt carrying it, which the first prompt did not.
        let prompts = provider.prompts.lock().unwrap().clone();
        let steered = format!("{STEERING_PREFIX}{INSTRUCTION}");
        assert!(
            !prompts[0].contains(&steered),
            "the instruction cannot appear before it was sent"
        );
        assert!(
            prompts
                .iter()
                .skip(1)
                .any(|prompt| prompt.contains(&steered)),
            "a step after the instruction must carry it, got prompts: {prompts:?}"
        );

        // The run is over, so its connection is gone and steering is refused
        // again — the registration lives exactly as long as the attachment.
        assert_eq!(
            steering.steer(run_id, "too late to steer".to_owned()),
            Err(SandboxSteerRefusal::NotAttached),
        );
    })
    .await
    .expect("test completed within its time bound");
}

/// Cancelling an unattached container child — its driver is gone, its lease
/// expired — terminalizes immediately and leaves the container's teardown
/// obligation enqueued in the same transaction, for the sweep to destroy.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_an_unattached_container_child_enqueues_its_teardown() {
    let (_dir, store, chat) = store().await;
    let run_id = admit_container_run(&store, chat.id, "orphaned by its driver").await;
    let run_uuid = *run_id.as_uuid();
    store
        .claim_container_agent_run(run_id, Uuid::new_v4(), chrono::Duration::milliseconds(1), 4)
        .await
        .unwrap()
        .expect("the dead driver's claim succeeds");
    store
        .begin_sandbox_provision(
            run_uuid,
            &SandboxTag::new().to_string(),
            chrono::Utc::now() + chrono::Duration::seconds(600),
            tidebreak_core::SandboxAdmissionMode::AttachedOnly,
        )
        .await
        .unwrap();
    assert!(store
        .commit_sandbox_provision_handle(run_uuid, "unattended-container")
        .await
        .unwrap());
    tokio::time::sleep(Duration::from_millis(50)).await;

    store
        .request_agent_run_cancellation(run_id)
        .await
        .unwrap()
        .expect("the cancellation request lands");

    let cancelled = store.get_agent_run(run_id).await.unwrap().unwrap();
    assert_eq!(cancelled.status, AgentRunStatus::Cancelled);
    let teardowns = store.list_sandbox_teardowns().await.unwrap();
    assert_eq!(teardowns.len(), 1);
    assert_eq!(teardowns[0].handle.as_deref(), Some("unattended-container"));
}

// --- Docker end-to-end (gated on a container runtime + the agent image) -------

/// Build the sandbox-agent image for the Docker-gated tests, returning its tag,
/// or `None` when no container runtime is present (there is none in the
/// unit-test sandbox; CI runners have Docker). A present daemon that fails the
/// build is a defect, not an environment to skip, so that panics.
///
/// Both Docker tests build the same tag; the CI lane serializes them so the
/// second build is a cache hit.
async fn build_agent_image() -> Option<&'static str> {
    use crate::sandbox_docker::DockerSandboxBackend;

    let backend_probe = DockerSandboxBackend::with_defaults();
    if !backend_probe.is_available() {
        eprintln!("skipping: no container runtime on PATH");
        return None;
    }

    // Build from the workspace root (the Dockerfile's build context is the
    // whole workspace, so Cargo.lock is visible to `--locked`).
    let image = "tidebreak-sandbox-agent:it";
    let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let build = tokio::time::timeout(
        Duration::from_secs(1800),
        tokio::process::Command::new("docker")
            .current_dir(&workspace_root)
            .args([
                "build",
                "-f",
                "crates/tidebreak-sandbox-agent/Dockerfile",
                "-t",
                image,
                ".",
            ])
            .output(),
    )
    .await;
    match build {
        Ok(Ok(output)) if output.status.success() => Some(image),
        Ok(Ok(output)) => {
            panic!(
                "building the agent image failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(Err(error)) => {
            eprintln!("skipping: could not invoke docker build: {error}");
            None
        }
        Err(_) => panic!("building the agent image timed out"),
    }
}

/// The full stack on a real Docker container: build the `tidebreak-sandbox-agent`
/// image, admit a container run, and drive it end to end — provision a container
/// from the image, attach over its published loopback port, answer its
/// `exec`-then-final model steps from a mock host model (so the container really
/// runs a shell command in its own boundary), and assert the result committed
/// exactly once and the container was torn down.
///
/// Skipped cleanly when no container runtime or daemon is present (there is none
/// in the unit-test sandbox); CI runners have Docker. Building the image is heavy
/// (it compiles the agent crate's slice of the workspace inside Docker), so this
/// is the one place that pays for it, guarded behind daemon detection and a long
/// timeout.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires a Docker daemon and builds the agent image; run explicitly or in the Docker CI lane"]
async fn docker_end_to_end_drives_a_real_container() {
    use crate::sandbox_docker::{DockerConfig, DockerSandboxBackend, RUN_TAG_LABEL};

    let Some(image) = build_agent_image().await else {
        return;
    };

    let (_dir, store, chat) = store().await;
    let run_id = admit_container_run(&store, chat.id, "count these four words now").await;

    let backend = Arc::new(DockerSandboxBackend::new(DockerConfig {
        image: image.to_owned(),
        ..DockerConfig::default()
    }));
    let provider = Arc::new(ScriptedProvider::new(vec![
        // Runs a real shell command inside the real container (in-container
        // execution is the containment).
        "use-tool:exec:{\"command\":\"echo count these four words\"}".to_owned(),
        "the count is four".to_owned(),
    ]));
    let resolver = Arc::new(FixedResolver(provider.clone()));

    let runner = SandboxContainerRunner::new(
        store.clone(),
        backend,
        resolver,
        SandboxContainerRunConfig {
            dial_timeout: Duration::from_secs(30),
            ..SandboxContainerRunConfig::default()
        },
    );
    let outcome = tokio::time::timeout(Duration::from_secs(120), runner.drive(run_id))
        .await
        .expect("driving a real container completes within its bound")
        .expect("driving succeeds")
        .expect("the container run is claimable");
    assert_eq!(outcome, SandboxContainerRunOutcome::Completed(run_id));

    let committed = store.get_agent_run(run_id).await.unwrap().unwrap();
    assert_eq!(committed.status, AgentRunStatus::Completed);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);

    // The container was torn down: no container carrying this run's tag remains.
    let listed = tokio::process::Command::new("docker")
        .args([
            "ps",
            "-a",
            "--filter",
            &format!("label={RUN_TAG_LABEL}"),
            "--filter",
            &format!("label=tidebreak.run-id={run_id}"),
            "--format",
            "{{.ID}}",
        ])
        .output()
        .await
        .unwrap();
    assert!(
        String::from_utf8_lossy(&listed.stdout).trim().is_empty(),
        "the container should have been torn down"
    );
}

/// Dial the container's published loopback port, retrying while it starts up.
async fn dial_container(authority: &str) -> tokio::net::TcpStream {
    for _ in 0..60 {
        if let Ok(Ok(stream)) = tokio::time::timeout(
            Duration::from_secs(2),
            tokio::net::TcpStream::connect(authority),
        )
        .await
        {
            return stream;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    panic!("could not dial the container at {authority}");
}

/// The packaged agent image conforms at the transport boundary.
///
/// This is the conformance slice #822 deferred until the container backend
/// existed: the in-process suite proves the reference implementation, and this
/// proves the supervisor actually shipped in the image, over a real published
/// port. Three scenarios are the ones a third-party host would hit first, and
/// the only ones the real (well-behaved) agent can exhibit:
///
/// 1. a version skew is answered with the sandbox's own version, then refused;
/// 2. a wrong transport secret is refused before any capability is served;
/// 3. the event stream resumes from a committed cursor across a reconnect,
///    redelivering an unacknowledged terminal event with its original sequence.
///
/// Scenarios that script sandbox-side misbehavior (ungranted capabilities,
/// over-bound frames, lane saturation) stay against the in-process reference —
/// the shipped agent does not misbehave on demand.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires a Docker daemon and builds the agent image; run explicitly or in the Docker CI lane"]
async fn docker_container_conforms_at_the_transport_boundary() {
    use crate::sandbox_docker::{DockerConfig, DockerSandboxBackend};
    use tidebreak_sandbox_protocol::{
        events::EventPayload, ids::EventCursor, protocol::AttachRequest, ConnectError, SandboxTag,
        WireClient,
    };

    let Some(image) = build_agent_image().await else {
        return;
    };

    let (_dir, store, _chat) = store().await;
    let backend = DockerSandboxBackend::new(DockerConfig {
        image: image.to_owned(),
        ..DockerConfig::default()
    });
    let run_id = RunId::new();
    let handle = backend
        .provision(ProvisionRequest {
            run_id,
            tag: SandboxTag::new(),
            lifetime_cap_secs: None,
            // Deny-all egress: the attach transport must work through the
            // proxy's relay even when the sandbox is granted no egress at all.
            network_policy: Default::default(),
        })
        .await
        .expect("provisioning a conformance container succeeds");

    // A failed assertion below leaks the container in a local run; the CI
    // runner is ephemeral, and local reruns reclaim it through the tag sweep.
    tokio::time::timeout(Duration::from_secs(120), async {
        let address = backend.address(&handle).await.expect("container address");
        let authority = address
            .base_url
            .trim_start_matches("http://")
            .to_owned();

        let host = CapabilityHost::new(
            GrantSet::new(
                RunProvenance {
                    run_id,
                    provider: "local-container".to_owned(),
                },
                [Capability::ModelInference],
            ),
            Arc::new(HostModelProxy {
                // No scripted directives: every completion defaults to a final
                // answer, so the agent emits progress then a terminal result.
                resolver: Arc::new(FixedResolver(Arc::new(ScriptedProvider::new(vec![])))),
                cancel: CancelToken::new(),
                lease_guard: None,
                config: AgentConfig::default(),
                spent: AtomicU32::new(0),
                budget: 24,
                accounting: None,
                observed: HostModelObservedAccounting::default(),
            }),
            Arc::new(DurableOperationStore::new(store.clone(), run_id)),
        );

        // 1. Version skew: answered with the sandbox's own version, refused,
        //    and the connection is not established.
        let Err(refusal) = WireClient::connect(
            dial_container(&authority).await,
            AttachRequest {
                protocol_version: PROTOCOL_VERSION + 1,
                run_id,
                resume_from: EventCursor::START,
                transport_secret: address.transport_secret.clone(),
            },
            host.clone(),
        )
        .await
        else {
            panic!("a version skew must be refused");
        };
        match refusal {
            ConnectError::VersionRefused(refused) => {
                assert_eq!(
                    refused.protocol_version, PROTOCOL_VERSION,
                    "the refusal must carry the sandbox's own version so the peer learns the mismatch"
                );
            }
            other => panic!("expected a version refusal, got: {other}"),
        }

        // 2. Wrong transport secret: refused before anything is served.
        let Err(refusal) = WireClient::connect(
            dial_container(&authority).await,
            AttachRequest {
                protocol_version: PROTOCOL_VERSION,
                run_id,
                resume_from: EventCursor::START,
                transport_secret: TransportSecret::new("not-the-minted-secret"),
            },
            host.clone(),
        )
        .await
        else {
            panic!("a wrong secret must be refused");
        };
        assert!(
            matches!(refusal, ConnectError::Unauthenticated(_)),
            "expected an authentication refusal, got: {refusal}"
        );

        // 3. Attach for real, take the stream to its terminal event, but leave
        //    that terminal event unacknowledged.
        let mut conn = WireClient::connect(
            dial_container(&authority).await,
            AttachRequest {
                protocol_version: PROTOCOL_VERSION,
                run_id,
                resume_from: EventCursor::START,
                transport_secret: address.transport_secret.clone(),
            },
            host.clone(),
        )
        .await
        .expect("an authenticated attach is accepted");
        // The packaged agent starts nothing until the run init arrives.
        conn.send_init(tidebreak_sandbox_protocol::init::RunInit {
            run_id,
            provenance: RunProvenance {
                run_id,
                provider: "local-container".to_owned(),
            },
            task: "answer briefly".to_owned(),
            deadline_unix_secs: 4_102_444_800,
            admission: tidebreak_sandbox_protocol::init::AdmissionMode::AttachedOnly,
            policy: tidebreak_sandbox_protocol::init::PolicySnapshot {
                egress_allowlist: Vec::new(),
                granted_capabilities: vec![Capability::ModelInference],
            },
            scoped_token: None,
        })
        .await;
        let first = conn.next_event().await.expect("the agent's first event");
        let mut committed = EventCursor::committed(first.sequence);
        conn.acknowledge(committed).await;
        let terminal = loop {
            let event = conn.next_event().await.expect("the stream reaches a terminal event");
            if matches!(
                event.payload,
                EventPayload::Result(_) | EventPayload::Failed(_)
            ) {
                break event;
            }
            committed = EventCursor::committed(event.sequence);
            conn.acknowledge(committed).await;
        };
        assert!(
            matches!(terminal.payload, EventPayload::Result(_)),
            "a directive-free completion must end the run with a result"
        );
        drop(conn);

        // Reattach from the last committed cursor: the sandbox must redeliver
        // the unacknowledged terminal event, same sequence, same payload.
        let mut conn = WireClient::connect(
            dial_container(&authority).await,
            AttachRequest {
                protocol_version: PROTOCOL_VERSION,
                run_id,
                resume_from: committed,
                transport_secret: address.transport_secret.clone(),
            },
            host.clone(),
        )
        .await
        .expect("a reattach after a disconnect is accepted");
        let redelivered = conn
            .next_event()
            .await
            .expect("the unacknowledged event is redelivered");
        assert_eq!(redelivered.sequence, terminal.sequence);
        assert_eq!(redelivered.payload, terminal.payload);
    })
    .await
    .expect("conformance checks completed within their bound");

    backend
        .destroy(&handle)
        .await
        .expect("tearing the conformance container down succeeds");
}

/// Run one Python probe inside the sandbox container and return its first
/// stdout line. The probe scripts are written to always exit 0 and print a
/// marker, so a failure is a wrong marker (with the probe's own diagnostics in
/// it), not an opaque non-zero exit.
async fn exec_probe(container: &str, script: &str) -> String {
    let output = tokio::process::Command::new("docker")
        .args(["exec", container, "python3", "-c", script])
        .output()
        .await
        .expect("docker exec runs");
    assert!(
        output.status.success(),
        "probe exited non-zero: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// The egress boundary on a real container: only the topology can prove that
/// the internal network actually has no route out, that the proxy's alias
/// resolves from the sandbox, and that the proxy's verdicts are what a command
/// inside the boundary observes. The loopback integration tests already prove
/// the proxy's protocol; this proves the wiring around it:
///
/// 1. a direct connection to a public address — ignoring `HTTP(S)_PROXY` —
///    fails: the internal network has no route anywhere;
/// 2. a CONNECT to a policy-denied destination is refused with a 403;
/// 3. a CONNECT to the policy's allowed host flows end to end (the proxy
///    resolves the name host-side and splices a real upstream connection);
/// 4. an external DNS lookup inside the sandbox fails — the embedded resolver
///    answers only the internal network's own names, and the sandbox's
///    upstream is the blackhole `sandbox_docker` configures.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires a Docker daemon, the agent image, and outbound network reach; run explicitly or in the Docker CI lane"]
async fn docker_egress_boundary_denies_and_allows_through_the_proxy() {
    use crate::sandbox_docker::{
        sandbox_name, DockerConfig, DockerSandboxBackend, EGRESS_PROXY_ALIAS, EGRESS_PROXY_PORT,
    };
    use tidebreak_sandbox_protocol::SandboxNetworkPolicy;

    let Some(image) = build_agent_image().await else {
        return;
    };

    // The one host the policy allows, on 443 only. A stable public site the CI
    // runner can reach; the allowed probe needs real outbound connectivity.
    const ALLOWED_HOST: &str = "example.com";
    const DENIED_HOST: &str = "denied.example";

    let backend = DockerSandboxBackend::new(DockerConfig {
        image: image.to_owned(),
        ..DockerConfig::default()
    });
    let tag = SandboxTag::new();
    let handle = backend
        .provision(ProvisionRequest {
            run_id: RunId::new(),
            tag,
            lifetime_cap_secs: None,
            network_policy: SandboxNetworkPolicy {
                allow_all_public: false,
                allowed_hosts: Vec::new(),
                https_only_hosts: vec![ALLOWED_HOST.to_owned()],
            },
        })
        .await
        .expect("provisioning the egress-probe sandbox succeeds");
    let sandbox = sandbox_name(tag);

    // A CONNECT probe: dial the proxy by its alias (retrying while the proxy
    // container finishes starting), issue one CONNECT, print the status line.
    let connect_probe = |host: &str| {
        format!(
            r#"
import socket, time
last = None
for _ in range(30):
    try:
        s = socket.create_connection(("{EGRESS_PROXY_ALIAS}", {EGRESS_PROXY_PORT}), timeout=5)
        break
    except OSError as error:
        last = error
        time.sleep(1)
else:
    print(f"PROXY UNREACHABLE {{last}}")
    raise SystemExit(0)
s.settimeout(30)
s.sendall(b"CONNECT {host}:443 HTTP/1.1\r\nHost: {host}:443\r\n\r\n")
print(s.recv(64).decode("latin1").splitlines()[0])
"#
        )
    };

    let checks = tokio::time::timeout(Duration::from_secs(180), async {
        // 1. The topology itself: a command that ignores the proxy environment
        // has no route to a public address, well-known or otherwise.
        let direct = exec_probe(
            &sandbox,
            r#"
import socket
try:
    socket.create_connection(("1.1.1.1", 443), timeout=10)
    print("REACHED")
except OSError as error:
    print(f"BLOCKED {error}")
"#,
        )
        .await;
        assert!(
            direct.starts_with("BLOCKED"),
            "direct egress must have no route: {direct}"
        );

        // 2. The proxy refuses a policy-denied destination before touching it.
        let denied = exec_probe(&sandbox, &connect_probe(DENIED_HOST)).await;
        assert!(
            denied.starts_with("HTTP/1.1 403"),
            "a denied CONNECT must be refused with 403: {denied}"
        );

        // 3. The allowed destination flows end to end through the proxy.
        let allowed = exec_probe(&sandbox, &connect_probe(ALLOWED_HOST)).await;
        assert!(
            allowed.starts_with("HTTP/1.1 200"),
            "an allowed CONNECT must be established: {allowed}"
        );

        // 4. The name-lookup side channel: an external lookup inside the
        // sandbox fails, because the proxy resolves destinations host-side and
        // the sandbox's own upstream is a blackhole.
        let lookup = exec_probe(
            &sandbox,
            &format!(
                r#"
import socket
try:
    socket.getaddrinfo("{ALLOWED_HOST}", 443)
    print("RESOLVED")
except OSError as error:
    print(f"UNRESOLVED {{error}}")
"#
            ),
        )
        .await;
        assert!(
            lookup.starts_with("UNRESOLVED"),
            "an external DNS lookup inside the sandbox must fail: {lookup}"
        );
    })
    .await;

    // A failed assertion above leaks the trio in a local run, exactly as in
    // the conformance test: the CI runner is ephemeral, and local reruns
    // reclaim it through the tag sweep. The timeout case still tears down.
    backend
        .destroy(&handle)
        .await
        .expect("tearing the egress-probe sandbox down succeeds");
    checks.expect("egress probes completed within their bound");
}
