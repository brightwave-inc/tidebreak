//! Supervised execution for durably claimed chat turns.

use std::fs;
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt as StdDirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use cap_std::ambient_authority;
use cap_std::fs::{Dir, DirBuilder};
#[cfg(unix)]
use cap_std::fs::{DirBuilderExt as CapDirBuilderExt, PermissionsExt as CapPermissionsExt};
use chrono::Utc;
use futures::channel::mpsc::{unbounded, UnboundedReceiver};
use futures::StreamExt;
use openwave_core::{
    Agent, AgentConfig, AgentError, AgentEvent, AgentRunExecutionLocation, AgentRunWaitCondition,
    AgentRunWaitSetCheckpointRequest, AgentTurnOutcome, BlobStore, CheckpointSandboxSpawnOutcome,
    ClaimedAgentEvent, CompleteTurnRunOutcome, ForegroundAgentWaitRequest, MessageId,
    ParkTurnForAgentRunWaitSetOutcome, ParkTurnForClientCallOutcome, RecordTurnFailureOutcome,
    Result, SandboxAgentSpawnRequest, SandboxSpawnCheckpointRequest, SecretProvider,
    SequencedEvent, Store, ToolRegistry, ToolScratch, TurnCheckpointProgress, TurnFailureRetry,
    TurnId, TurnRun, TurnRunStatus, SPAWN_SANDBOX_AGENT_TOOL, WAIT_FOR_AGENTS_TOOL,
};
use tokio::sync::Notify;

use crate::approvals::ApprovalBroker;
use crate::bus::EventBus;
use crate::chat_titling::ChatTitler;
use crate::exec_write_snapshot::TurnScratchJournal;
use crate::mcp_config::McpRuntime;
use crate::resolver::ProviderResolver;
use crate::retry::{LaneBackoff, RetryAttempt, RetrySchedule};
use crate::state::{BlobWriteGuard, TurnGuard};

#[derive(Debug, Clone, Copy)]
pub(crate) struct TurnWorkerConfig {
    pub(crate) lease: Duration,
    pub(crate) heartbeat: Duration,
    pub(crate) steer_poll: Duration,
    pub(crate) idle_min: Duration,
    pub(crate) idle_cap: Duration,
    pub(crate) failure_delay: Duration,
    /// Ceiling on the lane's own backoff after consecutive iteration errors,
    /// so a store outage is not polled at a fixed rate forever.
    pub(crate) failure_delay_cap: Duration,
    pub(crate) retry: RetrySchedule,
    pub(crate) max_concurrency: usize,
    /// Startup-resolved location for every background child admitted by this
    /// worker. Capability detection never runs in the spawn path.
    pub(crate) sandbox_spawn_execution_location: AgentRunExecutionLocation,
}

impl Default for TurnWorkerConfig {
    fn default() -> Self {
        Self {
            lease: Duration::from_secs(60),
            heartbeat: Duration::from_secs(15),
            steer_poll: Duration::from_millis(250),
            idle_min: Duration::from_millis(250),
            idle_cap: Duration::from_secs(5),
            failure_delay: Duration::from_secs(1),
            failure_delay_cap: Duration::from_secs(30),
            // A user is watching this turn: start retrying in well under a
            // second, and give up after ten minutes rather than hold a chat
            // open longer than anyone waits.
            retry: RetrySchedule::new(
                Duration::from_millis(250),
                Duration::from_secs(100),
                Duration::from_secs(600),
            ),
            max_concurrency: 4,
            sandbox_spawn_execution_location: AgentRunExecutionLocation::InProcess,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TurnWorkerOutcome {
    Completed(TurnId),
    WaitingForClient(TurnId),
    WaitingForAgentRun(TurnId),
    Resuming(TurnId),
    Cancelled(TurnId),
    Failed(TurnId),
    LeaseLost(TurnId),
}

#[derive(Clone)]
pub(crate) struct TurnWorker {
    store: Arc<dyn Store>,
    resolver: Arc<dyn ProviderResolver>,
    secrets: Arc<dyn SecretProvider>,
    /// The OS authority for managed-mode resolution, so background role
    /// resolution sees the same policy every request handler does.
    os_policy: Arc<dyn crate::managed_policy::OsPolicySource>,
    tools: Arc<ToolRegistry>,
    blobs: Option<Arc<dyn BlobStore>>,
    blob_writes: Option<Arc<BlobWriteGuard>>,
    mcp: Option<Arc<McpRuntime>>,
    approvals: Arc<ApprovalBroker>,
    events: Arc<EventBus>,
    signals: Arc<TurnGuard>,
    titler: Arc<ChatTitler>,
    wake: Arc<Notify>,
    sandbox_agent_wake: Arc<Notify>,
    agent_config: AgentConfig,
    exec_folder_context: Option<Arc<crate::code_execution::ConfiguredCodeExecutionProvider>>,
    private_scratch_root: Option<PathBuf>,
    config: TurnWorkerConfig,
}

enum EventAppend {
    Committed,
    Cancelling,
    LeaseLost,
}

enum ClaimAction {
    Idle,
    Terminalized,
    Claimed(Box<TurnRun>, uuid::Uuid),
}

enum HeartbeatOutcome {
    Cancelling,
    LeaseLost,
}

/// Exact foreground capability surface retained for one live turn execution.
///
/// Both the provider-visible tool definitions and host-owned operating prompt
/// derive from `tools`; runtime MCP refreshes can only produce a different
/// surface for a later execution.
pub(crate) struct ForegroundTurnSurface {
    pub(crate) tools: Arc<ToolRegistry>,
    pub(crate) agent_config: AgentConfig,
}

#[cfg(test)]
pub(crate) fn freeze_foreground_turn_surface(
    tools: Arc<ToolRegistry>,
    base_agent_config: &AgentConfig,
) -> ForegroundTurnSurface {
    freeze_foreground_turn_surface_with_folders(
        tools,
        base_agent_config,
        &[],
        &[],
        &[],
        &openwave_core::NetworkPolicy::default(),
        crate::code_execution::DEFAULT_TIMEOUT_MS,
        false,
        None,
        None,
        false,
        openwave_core::TurnWebSearch::Host,
    )
}

#[allow(clippy::too_many_arguments)]
fn freeze_foreground_turn_surface_with_folders(
    tools: Arc<ToolRegistry>,
    base_agent_config: &AgentConfig,
    exec_folders: &[crate::code_execution::ResolvedExecFolderGrant],
    skills: &[openwave_code_execution::SkillPackage],
    plugins: &[openwave_code_execution::PluginPackage],
    network_policy: &openwave_core::NetworkPolicy,
    exec_timeout_ms: u64,
    offline_package_cache: bool,
    office_rendering: Option<bool>,
    node_runtime: Option<openwave_code_execution::HostToolStatus>,
    plan_mode: bool,
    web_search: openwave_core::TurnWebSearch,
) -> ForegroundTurnSurface {
    let mut agent_config = base_agent_config.clone();
    // A chat-only model gets one genuinely empty capability snapshot. Keeping
    // the full host registry here would make the prompt describe tools the
    // request layer later withholds, and would still leave those tools
    // executable if the model emitted an unadvertised call.
    let tools = if agent_config.tools_supported {
        tools
    } else {
        Arc::new(ToolRegistry::new())
    };
    // The prompt describes the capabilities the turn actually has. A vendor
    // turn still has `web_search` — the provider runs it, but the model names
    // and uses it the same way — so only the turn that has no search at all
    // drops the section, and a turn that keeps it keeps the guidance that has
    // always come with it.
    let web_search = if agent_config.tools_supported {
        web_search
    } else {
        openwave_core::TurnWebSearch::Off
    };
    let mut specs = tools.specs_for_surface(true, plan_mode);
    if web_search == openwave_core::TurnWebSearch::Off {
        specs.retain(|spec| spec.name != openwave_core::WEB_SEARCH_TOOL);
    }
    agent_config.web_search = web_search;
    agent_config.system_prompt = Some(crate::foreground_prompt::compose_for_surface(
        &specs,
        exec_folders,
        skills,
        plugins,
        network_policy,
        exec_timeout_ms,
        offline_package_cache,
        office_rendering,
        node_runtime,
        plan_mode,
    ));
    ForegroundTurnSurface {
        tools,
        agent_config,
    }
}

/// Finish live publication for state transitions that committed before this
/// worker learned it had lost the lease. The producer must be stopped first so
/// the channel closes after its already-buffered emissions are consumed.
async fn drain_committed_events(
    events: &EventBus,
    chat_id: openwave_core::ChatId,
    emissions: &mut UnboundedReceiver<ClaimedAgentEvent>,
) {
    while let Some(emission) = emissions.next().await {
        match emission {
            ClaimedAgentEvent::Committed { event, .. } => {
                let _ = events.sender(chat_id).send(event);
            }
            ClaimedAgentEvent::Recovered { .. } => {}
            ClaimedAgentEvent::Flush(acknowledge) => {
                let _ = acknowledge.send(());
            }
            ClaimedAgentEvent::Pending { .. } => {}
        }
    }
}

fn client_checkpoint_is_valid(
    tools: &ToolRegistry,
    chat_id: openwave_core::ChatId,
    turn_id: TurnId,
    request: &openwave_core::ClientToolCallRequest,
) -> bool {
    request.chat_id == chat_id
        && request.turn_id == turn_id
        && request.is_well_formed()
        && tools.client_arguments_are_valid(&request.name, &request.arguments)
        && tools.execution(&request.name) == Some(openwave_core::ToolCallExecution::Client)
}

fn sandbox_spawn_checkpoint_is_valid(
    tools: &ToolRegistry,
    request: &SandboxAgentSpawnRequest,
) -> bool {
    request.is_well_formed()
        && tools.is_foreground_sandbox_spawn(SPAWN_SANDBOX_AGENT_TOOL)
        && tools
            .sandbox_spawn_task(
                SPAWN_SANDBOX_AGENT_TOOL,
                &serde_json::json!({"task": request.task}),
            )
            .is_some_and(|task| task == request.task)
}

fn agent_wait_checkpoint_is_valid(
    tools: &ToolRegistry,
    request: &ForegroundAgentWaitRequest,
) -> bool {
    request.is_well_formed()
        && tools.is_foreground_agent_wait(WAIT_FOR_AGENTS_TOOL)
        && tools
            .wait_for_agent_ids(WAIT_FOR_AGENTS_TOOL, &request.arguments)
            .is_some_and(|ids| ids == request.child_run_ids)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LeaseState {
    Running,
    Cancelling,
    Lost,
}

enum LiveTurnState {
    Running,
    Cancelling,
    Lost,
}

enum ResolutionState {
    Retry,
    /// Boxed: an event carrying a tool's projected command output dwarfs the
    /// other variants, and this enum is returned on every resolution poll.
    Resolved(Box<SequencedEvent>),
    Cancelling,
    Lost,
}

enum TerminalIdentity<'a> {
    Completed {
        output: &'a openwave_core::Message,
        citations: &'a [openwave_core::AssistantCitationInput],
        event: &'a AgentEvent,
    },
    Failed {
        code: &'a str,
        detail: &'a str,
        event: &'a AgentEvent,
    },
    Cancelled {
        event: &'a AgentEvent,
    },
}

impl TerminalIdentity<'_> {
    fn status(&self) -> TurnRunStatus {
        match self {
            Self::Completed { .. } => TurnRunStatus::Completed,
            Self::Failed { .. } => TurnRunStatus::Failed,
            Self::Cancelled { .. } => TurnRunStatus::Cancelled,
        }
    }

    fn event(&self) -> &AgentEvent {
        match self {
            Self::Completed { event, .. }
            | Self::Failed { event, .. }
            | Self::Cancelled { event } => event,
        }
    }

    fn matches_turn(&self, turn: &TurnRun) -> bool {
        match self {
            Self::Completed { output, .. } => turn.output_message_id == Some(output.id),
            Self::Failed { code, detail, .. } => {
                turn.last_error_code.as_deref() == Some(*code)
                    && turn.last_error_detail.as_deref() == Some(*detail)
            }
            Self::Cancelled { .. } => true,
        }
    }
}

struct AbortOnDrop<T>(tokio::task::JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl<T> AbortOnDrop<T> {
    async fn abort_and_wait(&mut self) {
        self.0.abort();
        let _ = (&mut self.0).await;
    }
}

impl TurnWorker {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        store: Arc<dyn Store>,
        resolver: Arc<dyn ProviderResolver>,
        secrets: Arc<dyn SecretProvider>,
        os_policy: Arc<dyn crate::managed_policy::OsPolicySource>,
        tools: Arc<ToolRegistry>,
        approvals: Arc<ApprovalBroker>,
        events: Arc<EventBus>,
        signals: Arc<TurnGuard>,
        wake: Arc<Notify>,
        sandbox_agent_wake: Arc<Notify>,
        agent_config: AgentConfig,
        private_scratch_root: Option<PathBuf>,
        config: TurnWorkerConfig,
    ) -> Self {
        assert!(!config.lease.is_zero());
        assert!(!config.heartbeat.is_zero());
        assert!(!config.steer_poll.is_zero());
        assert!(config.heartbeat < config.lease);
        assert!(config.max_concurrency > 0);
        let titler = Arc::new(ChatTitler::new(
            store.clone(),
            resolver.clone(),
            events.clone(),
        ));
        Self {
            store,
            resolver,
            secrets,
            os_policy,
            tools,
            blobs: None,
            blob_writes: None,
            mcp: None,
            approvals,
            events,
            signals,
            titler,
            wake,
            sandbox_agent_wake,
            agent_config,
            exec_folder_context: None,
            private_scratch_root,
            config,
        }
    }

    /// Hydrate image attachments for outbound requests from `blobs`.
    ///
    /// Without it an agent evicts every image block to a text stand-in, so a
    /// turn still runs but the model is told the image is unavailable.
    pub(crate) fn with_blobs(mut self, blobs: Arc<dyn BlobStore>) -> Self {
        self.blobs = Some(blobs);
        self
    }

    /// Retain the bytes a structured scratch write replaces, so an overwrite in
    /// private scratch is recoverable the way a folder write-back already is.
    ///
    /// Without the blob write guard there is nowhere safe to publish the
    /// retained copy, so the journal stays off rather than racing the retirer.
    pub(crate) fn with_blob_write_locks(mut self, blob_writes: Arc<BlobWriteGuard>) -> Self {
        self.blob_writes = Some(blob_writes);
        self
    }

    /// Attach this turn's scratch-overwrite journal, when the runtime has the
    /// durable stores to keep one.
    fn with_scratch_write_journal(
        &self,
        scratch: ToolScratch,
        folder: PathBuf,
        chat_id: openwave_core::ChatId,
        turn_id: TurnId,
    ) -> ToolScratch {
        let Some((blobs, blob_writes)) = self.blobs.clone().zip(self.blob_writes.clone()) else {
            return scratch;
        };
        scratch.with_write_journal(Arc::new(TurnScratchJournal::new(
            self.store.clone(),
            blobs,
            blob_writes,
            folder,
            chat_id,
            turn_id,
        )))
    }

    /// Resolve one immutable registry when a turn begins. Runtime MCP changes
    /// affect later turns without changing this worker's active replay surface.
    pub(crate) fn with_mcp_runtime(mut self, mcp: Arc<McpRuntime>) -> Self {
        self.mcp = Some(mcp);
        self
    }

    /// Add per-turn folder visibility for local exec. The provider resolves
    /// again at invocation time; this snapshot is model guidance, not
    /// authority.
    pub(crate) fn with_exec_folder_context(
        mut self,
        provider: Arc<crate::code_execution::ConfiguredCodeExecutionProvider>,
    ) -> Self {
        self.exec_folder_context = Some(provider);
        self
    }

    pub(crate) async fn run(self) {
        let mut turns = tokio::task::JoinSet::new();
        let mut idle_delay = self.config.idle_min;
        let mut failure_backoff =
            LaneBackoff::new(self.config.failure_delay, self.config.failure_delay_cap);
        let mut pending_claim_token = None;
        loop {
            let mut scan_failed = false;
            while turns.len() < self.config.max_concurrency {
                let lease_token = *pending_claim_token.get_or_insert_with(uuid::Uuid::new_v4);
                match self.claim_once(lease_token).await {
                    Ok(ClaimAction::Claimed(turn, lease_token)) => {
                        pending_claim_token = None;
                        let worker = self.clone();
                        turns.spawn(async move { worker.process(*turn, lease_token).await });
                        idle_delay = self.config.idle_min;
                    }
                    Ok(ClaimAction::Terminalized) => {
                        pending_claim_token = None;
                        idle_delay = self.config.idle_min;
                    }
                    Ok(ClaimAction::Idle) => {
                        pending_claim_token = None;
                        break;
                    }
                    Err(error) => {
                        eprintln!("openwave: turn worker claim failed: {error}");
                        scan_failed = true;
                        break;
                    }
                }
            }

            if turns.len() == self.config.max_concurrency {
                if let Some(result) = turns.join_next().await {
                    log_turn_result(result);
                }
                idle_delay = self.config.idle_min;
                continue;
            }

            let delay = if scan_failed {
                failure_backoff.next_delay()
            } else {
                failure_backoff.reset();
                idle_delay
            };
            tokio::select! {
                result = turns.join_next(), if !turns.is_empty() => {
                    if let Some(result) = result {
                        log_turn_result(result);
                    }
                    idle_delay = self.config.idle_min;
                }
                _ = tokio::time::sleep(delay) => {
                    if !scan_failed {
                        idle_delay = idle_delay.saturating_mul(2).min(self.config.idle_cap);
                    }
                }
                _ = self.wake.notified() => {
                    idle_delay = self.config.idle_min;
                }
            }
        }
    }

    async fn claim_once(&self, lease_token: uuid::Uuid) -> Result<ClaimAction> {
        let now = Utc::now();
        let lease_expires_at = now + chrono_duration(self.config.lease)?;
        let action = self
            .store
            .claim_turn_run(lease_token, now, lease_expires_at)
            .await?;
        if let Some(terminal) = action.terminal_event {
            self.publish(terminal.chat_id, terminal.event);
            self.wake.notify_one();
            return Ok(ClaimAction::Terminalized);
        }
        let Some(turn) = action.turn else {
            return Ok(ClaimAction::Idle);
        };
        self.wake.notify_one();
        Ok(ClaimAction::Claimed(Box::new(turn), lease_token))
    }

    /// Run one claimed turn and close out whatever the turn staged.
    ///
    /// Exec writes into a granted folder land in a per-turn overlay, and the
    /// end of the turn is the only place that can apply them. Every way the run
    /// below returns has to pass through here, which is why the run itself is a
    /// separate function rather than an early return in this one.
    async fn process(&self, turn: TurnRun, lease_token: uuid::Uuid) -> Result<TurnWorkerOutcome> {
        let chat_id = turn.chat_id;
        let outcome = self.run_turn(turn, lease_token).await;
        if let Some(provider) = self.exec_folder_context.as_ref() {
            if let Some(turn_id) = provider.close_write_overlay(chat_id).await {
                self.events.publish_metadata(
                    chat_id,
                    crate::bus::ChatMetadataNotice::FileChangesRecorded { turn_id },
                );
            }
        }
        outcome
    }

    async fn run_turn(&self, turn: TurnRun, lease_token: uuid::Uuid) -> Result<TurnWorkerOutcome> {
        if turn.status != TurnRunStatus::Running || turn.lease_token != Some(lease_token) {
            return Err(AgentError::msg(format!(
                "claimed turn {} has an invalid execution identity",
                turn.id
            )));
        }
        let tools = self
            .mcp
            .as_ref()
            .map_or_else(|| self.tools.clone(), |mcp| mcp.snapshot());
        let mut total_model_steps = turn.model_steps;
        let consumed_steps = usize::try_from(total_model_steps).map_err(|_| {
            AgentError::msg(format!(
                "turn {} has invalid durable model-step accounting",
                turn.id
            ))
        })?;
        let Some(mut remaining_steps) = self.agent_config.max_steps.checked_sub(consumed_steps)
        else {
            return self
                .record_failure(
                    &turn,
                    lease_token,
                    total_model_steps,
                    turn.usage,
                    "invalid_turn_progress",
                    "durable model-step accounting exceeds the configured turn budget",
                )
                .await;
        };
        let mut total_usage = turn.usage;
        let mut checkpoint_usage = openwave_core::Usage::default();
        let mut checkpoint_steps = 0_usize;
        let mut continuation_instruction = None;
        // A spawn batch the previous claim segment parked on is picked up
        // here, before any provider call: the model already named these
        // delegations, and re-asking it would orphan the tool calls it
        // streamed for them. The turn's durable steer revision is the one the
        // resumed gate has to agree with, exactly as a live generation reads
        // it before checkpointing.
        let mut pending_sandbox_spawns = self
            .store
            .resumed_sandbox_spawn_batch(turn.id, turn.attempt_count, turn.claim_count)
            .await?;
        let mut pending_sandbox_spawn_steer_revision =
            (!pending_sandbox_spawns.is_empty()).then_some(turn.steer_revision);
        // A segment arriving with zero remaining steps is not refused: the
        // budget was spent by earlier segments — a checkpoint parked on the
        // last budgeted step, or a wrap-up whose provider call failed
        // retryably — and the agent runs the wrap-up-only attempt those
        // segments still owe the user (#1181). The wrap-up is outside the
        // budget by design, so `max_steps = 0` admits exactly one tool-free
        // model call and nothing else. Only a turn that never had a budget in
        // the first place stays a hard failure: there is no work to wrap up.
        if remaining_steps == 0 && consumed_steps == 0 {
            return self
                .record_failure(
                    &turn,
                    lease_token,
                    total_model_steps,
                    turn.usage,
                    "max_steps_exceeded",
                    "the turn was configured with no step budget",
                )
                .await;
        }
        let Some(mut chat) = self.store.get_chat(turn.chat_id).await? else {
            return self
                .record_failure(
                    &turn,
                    lease_token,
                    total_model_steps,
                    turn.usage,
                    "chat_missing",
                    "claimed turn chat is missing",
                )
                .await;
        };
        // A managed permission-mode ceiling binds at the gate, not only the
        // picker: a stored mode that predates the policy, or one written past
        // the route check, is clamped here before anything downstream reads
        // it. Resolved per turn, like the model, so an MDM push takes effect
        // on the next turn without a restart.
        let managed = crate::managed_policy::resolve(&*self.store, &*self.os_policy).await?;
        chat.permission_mode = managed.clamp_permission_mode(chat.permission_mode);
        let exec_folders = match self.exec_folder_context.as_ref() {
            Some(provider) => match provider.folder_grants_for_chat(&chat, turn.id).await {
                Ok(folders) => folders,
                Err(error) => {
                    // Prompt enrichment is not an authority boundary. A broker
                    // failure here leaves the list empty; the provider resolves
                    // again and returns the concrete error if `exec` is called.
                    eprintln!(
                        "openwave: local exec folders unavailable for chat {}: {}",
                        chat.id, error
                    );
                    Vec::new()
                }
            },
            None => Vec::new(),
        };
        let (skills, plugins) = match self.exec_folder_context.as_ref() {
            Some(provider) => {
                // The prompt's skill catalog directs the model to read each
                // staged SKILL.md before its first exec, so the workspace has
                // to be staged now — waiting for the first exec to stage it
                // loses that race and the read fails with not-found.
                provider.stage_turn_workspace(chat.id).await;
                (
                    provider.skill_catalog().await,
                    provider.plugin_catalog().await,
                )
            }
            None => (Vec::new(), Vec::new()),
        };
        let offline_package_cache = match self.exec_folder_context.as_ref() {
            Some(provider) => provider.offline_package_cache_ready().await,
            None => false,
        };
        let office_rendering = match self.exec_folder_context.as_ref() {
            Some(provider) => provider.office_rendering_available().await,
            None => None,
        };
        let node_runtime = match self.exec_folder_context.as_ref() {
            Some(provider) => provider.node_runtime_status().await,
            None => None,
        };
        let exec_timeout_ms = match self.exec_folder_context.as_ref() {
            Some(provider) => provider.current_timeout_ms().await,
            None => crate::code_execution::DEFAULT_TIMEOUT_MS,
        };
        let model_policy = if self.resolver.enforces_model_registry() {
            crate::providers::resolve_model_policy(&*self.store, &turn.model, true).await?
        } else {
            None
        };
        if model_policy.is_none() && self.resolver.enforces_model_registry() {
            return self
                .record_failure(
                    &turn,
                    lease_token,
                    total_model_steps,
                    total_usage,
                    "unknown_model",
                    "the turn's model is no longer registered for its provider",
                )
                .await;
        }
        let mut turn_agent_config = self.agent_config.clone();
        if let Some(policy) = model_policy.as_ref() {
            crate::providers::apply_model_policy(
                &mut turn_agent_config,
                policy,
                chat.reasoning_effort,
            )?;
        } else {
            crate::providers::apply_free_form_model(
                &mut turn_agent_config,
                turn.model.clone(),
                chat.reasoning_effort,
            )?;
        }
        // Resolved per turn, alongside the model: which search this turn gets
        // depends on both host policy and the model that is about to run, and
        // both can change between turns of one chat. A model the registry does
        // not own claims no vendor search. It is resolved before the surface is
        // frozen because the surface's prompt has to describe it.
        let web_search = if turn_agent_config.tools_supported {
            crate::web_search::resolve_turn_web_search(
                &*self.store,
                &*self.secrets,
                model_policy
                    .as_ref()
                    .is_some_and(|policy| policy.supports_vendor_web_search),
            )
            .await?
        } else {
            openwave_core::TurnWebSearch::Off
        };
        let surface = freeze_foreground_turn_surface_with_folders(
            tools,
            &turn_agent_config,
            &exec_folders,
            &skills,
            &plugins,
            &chat.network_policy,
            exec_timeout_ms,
            offline_package_cache,
            office_rendering,
            node_runtime,
            matches!(
                chat.permission_mode,
                Some(openwave_core::PermissionMode::Plan)
            ),
            web_search,
        );
        if let Some(prompt) = surface.agent_config.system_prompt.as_deref() {
            eprintln!(
                "openwave: turn {} operating_prompt={}",
                turn.id,
                crate::foreground_prompt::identity(prompt)
            );
        }
        // Resolved per turn, not at boot, so enabling a provider takes effect on
        // the next turn. `None` is not a failure: background maintenance is
        // skipped rather than run on the model the user picked for talking.
        let utility_model = if self.resolver.enforces_model_registry() {
            crate::model_roles::resolve_utility_model(
                &*self.store,
                &*self.secrets,
                &*self.os_policy,
            )
            .await?
        } else {
            None
        };
        // Named from the front of the turn rather than from its completion arms:
        // one hook point instead of two, and the title usually lands while the
        // assistant is still streaming its first answer.
        if chat.title.is_none() {
            if let Some(utility) = utility_model.clone() {
                self.titler.spawn(chat.id, utility);
            }
        }
        let active = loop {
            if let Some(active) = self.signals.register(turn.chat_id, turn.id, lease_token) {
                break active;
            }
            tokio::select! {
                () = self.signals.wait_until_vacant(turn.chat_id) => {}
                () = tokio::time::sleep(self.config.heartbeat) => {
                    match self.renew_lease(&turn, lease_token).await {
                        LeaseState::Running => {}
                        LeaseState::Cancelling => {
                            return self
                                .acknowledge_cancellation(&turn, lease_token, total_usage)
                                .await;
                        }
                        LeaseState::Lost => {
                            return Ok(TurnWorkerOutcome::LeaseLost(turn.id));
                        }
                    }
                }
            }
        };
        let cancel = active.cancel_token();
        match self.renew_lease(&turn, lease_token).await {
            LeaseState::Running => {}
            LeaseState::Cancelling => {
                drop(active);
                return self
                    .acknowledge_cancellation(&turn, lease_token, total_usage)
                    .await;
            }
            LeaseState::Lost => return Ok(TurnWorkerOutcome::LeaseLost(turn.id)),
        }
        let started = AgentEvent::TurnStarted { turn_id: turn.id };
        match self.append_event(&turn, lease_token, 1, &started).await? {
            EventAppend::Committed => {}
            EventAppend::Cancelling => {
                drop(active);
                return self
                    .acknowledge_cancellation(&turn, lease_token, total_usage)
                    .await;
            }
            EventAppend::LeaseLost => return Ok(TurnWorkerOutcome::LeaseLost(turn.id)),
        }

        let mut ordinal = 2_i32;
        let output_message_id = MessageId::new();
        loop {
            let mut heartbeat = AbortOnDrop(tokio::spawn(self.clone().heartbeat_lease(
                turn.clone(),
                lease_token,
                cancel.clone(),
            )));
            let mut heartbeat_open = true;
            let mut config = surface.agent_config.clone();
            config.utility_model = utility_model.clone();
            config.compaction = crate::routes::read_compaction_policy(&*self.store).await?;
            config.max_steps = remaining_steps;
            config.tool_scratch = self.private_scratch_root.as_deref().and_then(|root| {
                match private_chat_scratch(root, chat.id) {
                    Ok(scratch) => Some(self.with_scratch_write_journal(
                        scratch,
                        private_chat_scratch_path(root, chat.id),
                        chat.id,
                        turn.id,
                    )),
                    Err(error) => {
                        eprintln!(
                            "openwave: private scratch unavailable for chat {}: {}",
                            chat.id, error
                        );
                        None
                    }
                }
            });
            let provider = self.resolver.resolve().await;
            let steer = active.steer_inbox();
            let mut agent = Agent::new(provider, surface.tools.clone(), self.store.clone(), config)
                .with_approvals(self.approvals.clone())
                .with_cancel(cancel.clone())
                .with_steer(steer.clone())
                .with_durable_steer(lease_token)
                .with_foreground_agent_orchestration()
                .with_continuation_instruction(continuation_instruction.clone())
                .with_pending_sandbox_spawns(
                    pending_sandbox_spawns.clone(),
                    pending_sandbox_spawn_steer_revision,
                );
            if let Some(blobs) = self.blobs.clone() {
                agent = agent.with_blobs(blobs);
            }
            let chat = chat.clone();
            let (events_tx, mut events_rx) = unbounded();
            let mut drive = AbortOnDrop(tokio::spawn(async move {
                agent
                    .run_claimed_turn(&chat, turn.id, output_message_id, ordinal, &events_tx)
                    .await
            }));
            let mut drive_result = None;
            let mut channel_open = true;
            let mut steer_poll = tokio::time::interval(self.config.steer_poll);
            steer_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

            while drive_result.is_none() || channel_open {
                tokio::select! {
                    result = &mut drive.0, if drive_result.is_none() => {
                        drive_result = Some(result);
                    }
                    emission = events_rx.next(), if channel_open => {
                        match emission {
                            Some(ClaimedAgentEvent::Pending { ordinal: event_ordinal, event }) => {
                                if event_ordinal != ordinal {
                                    return Err(AgentError::msg(format!(
                                        "turn {} emitted event ordinal {event_ordinal}, expected {ordinal}",
                                        turn.id
                                    )));
                                }
                                match self.append_event(&turn, lease_token, event_ordinal, &event).await? {
                                    EventAppend::Committed => {
                                        ordinal = ordinal.checked_add(1).ok_or_else(|| {
                                            AgentError::msg(format!("turn {} event ordinal exhausted", turn.id))
                                        })?;
                                    }
                                    EventAppend::Cancelling => {
                                        // The agent assigns ordinals before enqueueing. This
                                        // emission is consumed even though cancellation won its
                                        // append, so advance past it before draining any already
                                        // buffered emissions. A later atomically committed event
                                        // may legitimately follow this journal gap.
                                        ordinal = ordinal.checked_add(1).ok_or_else(|| {
                                            AgentError::msg(format!(
                                                "turn {} event ordinal exhausted",
                                                turn.id
                                            ))
                                        })?;
                                        cancel.cancel();
                                    }
                                    EventAppend::LeaseLost => {
                                        drive.abort_and_wait().await;
                                        if heartbeat_open {
                                            heartbeat.abort_and_wait().await;
                                        }
                                        drain_committed_events(
                                            &self.events,
                                            turn.chat_id,
                                            &mut events_rx,
                                        )
                                        .await;
                                        return Ok(TurnWorkerOutcome::LeaseLost(turn.id));
                                    }
                                }
                            }
                            Some(ClaimedAgentEvent::Committed { ordinal: event_ordinal, event }) => {
                                if event_ordinal != ordinal {
                                    return Err(AgentError::msg(format!(
                                        "turn {} committed event ordinal {event_ordinal}, expected {ordinal}",
                                        turn.id
                                    )));
                                }
                                self.publish(turn.chat_id, event);
                                ordinal = ordinal.checked_add(1).ok_or_else(|| {
                                    AgentError::msg(format!("turn {} event ordinal exhausted", turn.id))
                                })?;
                            }
                            Some(ClaimedAgentEvent::Recovered { ordinal: event_ordinal, event: _ }) => {
                                if event_ordinal != ordinal {
                                    return Err(AgentError::msg(format!(
                                        "turn {} recovered event ordinal {event_ordinal}, expected {ordinal}",
                                        turn.id
                                    )));
                                }
                                ordinal = ordinal.checked_add(1).ok_or_else(|| {
                                    AgentError::msg(format!("turn {} event ordinal exhausted", turn.id))
                                })?;
                            }
                            Some(ClaimedAgentEvent::Flush(acknowledge)) => {
                                let _ = acknowledge.send(());
                            }
                            None => channel_open = false,
                        }
                    }
                    result = &mut heartbeat.0, if heartbeat_open => {
                        match result {
                            Ok(HeartbeatOutcome::Cancelling) => heartbeat_open = false,
                            Ok(HeartbeatOutcome::LeaseLost) | Err(_) => {
                                drive.abort_and_wait().await;
                                drain_committed_events(
                                    &self.events,
                                    turn.chat_id,
                                    &mut events_rx,
                                )
                                .await;
                                return Ok(TurnWorkerOutcome::LeaseLost(turn.id));
                            }
                        }
                    }
                    _ = steer_poll.tick(), if drive_result.is_none() => {
                        match self
                            .store
                            .list_pending_turn_steers(turn.id, lease_token, Utc::now())
                            .await
                        {
                            Ok(Some(pending)) if !pending.is_empty() => {
                                steer.signal_durable(pending.iter().any(|item| item.interrupt));
                            }
                            Ok(_) => {}
                            Err(error) => {
                                eprintln!(
                                    "openwave: turn {} steer poll failed: {error}",
                                    turn.id
                                );
                            }
                        }
                    }
                }
            }
            if heartbeat_open {
                heartbeat.abort_and_wait().await;
            }
            let drive_result =
                match drive_result.expect("drive completed before its channel closed") {
                    Ok(result) => result,
                    Err(error) => Err(AgentError::msg(format!("agent task stopped: {error}"))),
                };
            // The attempt consumed the whole queue: every requeued spawn either
            // passed the approval gate — and comes back as the outcome's
            // remaining requests — or was refused and answered durably. Holding
            // the old list would replay refusals on the next iteration.
            let had_pending_spawns = !pending_sandbox_spawns.is_empty();
            pending_sandbox_spawns.clear();
            pending_sandbox_spawn_steer_revision = None;
            match self.renew_lease(&turn, lease_token).await {
                LeaseState::Running => {}
                LeaseState::Cancelling => {
                    let usage = match &drive_result {
                        Ok(AgentTurnOutcome::Completed { usage, .. })
                        | Ok(AgentTurnOutcome::Cancelled { usage, .. })
                        | Ok(AgentTurnOutcome::ClientToolCall { usage, .. })
                        | Ok(AgentTurnOutcome::SandboxAgentSpawn { usage, .. })
                        | Ok(AgentTurnOutcome::WaitForAgents { usage, .. })
                        | Ok(AgentTurnOutcome::Failed { usage, .. }) => *usage,
                        Err(_) => openwave_core::Usage::default(),
                    };
                    match checked_usage_sum(total_usage, usage) {
                        Ok(total) => total_usage = total,
                        Err(error) => eprintln!(
                            "openwave: turn {} cancellation usage overflowed; acknowledging the durable baseline: {error}",
                            turn.id
                        ),
                    }
                    drop(active);
                    // A cancel that raced the drive still keeps the prose the
                    // cancelled outcome carried out of the loop.
                    let partial = match drive_result {
                        Ok(AgentTurnOutcome::Cancelled {
                            output, citations, ..
                        }) => output.map(|message| (message, citations)),
                        _ => None,
                    };
                    return self
                        .acknowledge_cancellation_with_output(
                            &turn,
                            lease_token,
                            total_usage,
                            partial,
                        )
                        .await;
                }
                LeaseState::Lost => return Ok(TurnWorkerOutcome::LeaseLost(turn.id)),
            }

            match drive_result {
                Ok(AgentTurnOutcome::Completed {
                    output,
                    citations,
                    usage,
                    stop_reason,
                    refusal,
                    steer_revision,
                    model_steps,
                }) => {
                    // A wrap-up-only attempt — dispatched with a zero budget —
                    // is the one shape that legitimately reports zero steps.
                    if model_steps > remaining_steps || (model_steps == 0 && remaining_steps != 0) {
                        return Err(AgentError::msg(format!(
                            "turn {} returned an invalid model-step count {model_steps}",
                            turn.id
                        )));
                    }
                    remaining_steps -= model_steps;
                    total_model_steps = checked_model_step_sum(total_model_steps, model_steps)?;
                    total_usage = match checked_usage_sum(total_usage, usage) {
                        Ok(total) => total,
                        Err(_) => {
                            return self
                                .record_failure(
                                    &turn,
                                    lease_token,
                                    total_model_steps,
                                    total_usage,
                                    "usage_overflow",
                                    "provider usage exceeded the supported turn total",
                                )
                                .await;
                        }
                    };
                    checkpoint_usage = match checked_usage_sum(checkpoint_usage, usage) {
                        Ok(total) => total,
                        Err(_) => {
                            return self
                                .record_failure(
                                    &turn,
                                    lease_token,
                                    total_model_steps,
                                    total_usage,
                                    "usage_overflow",
                                    "provider usage exceeded the supported checkpoint total",
                                )
                                .await;
                        }
                    };
                    checkpoint_steps =
                        checkpoint_steps.checked_add(model_steps).ok_or_else(|| {
                            AgentError::msg(format!(
                                "turn {} checkpoint model-step count overflowed",
                                turn.id
                            ))
                        })?;
                    if output.content.contains('\0') {
                        return self
                            .record_failure(
                                &turn,
                                lease_token,
                                total_model_steps,
                                total_usage,
                                "invalid_agent_output",
                                "agent output contained a NUL character",
                            )
                            .await;
                    }
                    // A model step that ends the turn with neither prose nor a
                    // tool call is not an answer, and completing on it hands the
                    // user a blank turn that looks successful and gives them
                    // nothing to act on. Two exemptions are deliberate: a
                    // refusal is its own outcome and stays meaningful with no
                    // prose behind it, and a step that only calls tools never
                    // reaches here — the loop runs the calls and takes another
                    // step. Assistant text persisted by earlier steps is already
                    // durable and is not touched by failing here.
                    if refusal.is_none() && output.content.trim().is_empty() {
                        if remaining_steps == 0 {
                            // The step budget is spent, so this was the turn's
                            // closing call. A retry would reclaim the turn only
                            // to fail immediately on the exhausted budget, so
                            // report the budget as the reason it ended with
                            // nothing to say.
                            return self
                                .record_failure(
                                    &turn,
                                    lease_token,
                                    total_model_steps,
                                    total_usage,
                                    "max_steps_exceeded",
                                    "the turn's closing model call returned no output after its step budget was spent",
                                )
                                .await;
                        }
                        // Budget is left, so asking again is the remedy: the
                        // retry rebuilds the same transcript, tool results
                        // included, and only re-runs the model call that came
                        // back empty. Once the attempt budget is spent the user
                        // gets a transient failure rather than a blank success.
                        return self
                            .record_classified_failure(
                                &turn,
                                lease_token,
                                total_model_steps,
                                total_usage,
                                "empty_model_response",
                                "the model returned neither text nor a tool call",
                                None,
                            )
                            .await;
                    }
                    let expected_steer_revision = steer_revision.ok_or_else(|| {
                        AgentError::msg(format!(
                            "turn {} completed without a durable generation fence",
                            turn.id
                        ))
                    })?;
                    if (stop_reason == openwave_core::StopReason::Refusal) != refusal.is_some() {
                        return Err(AgentError::msg(format!(
                            "turn {} returned inconsistent refusal metadata",
                            turn.id
                        )));
                    }
                    let terminal_event = match refusal.clone() {
                        Some(refusal) => AgentEvent::TurnRefused {
                            usage: total_usage,
                            refusal,
                        },
                        None => AgentEvent::TurnCompleted {
                            usage: total_usage,
                            stop_reason,
                        },
                    };
                    let continue_after_steer = loop {
                        let completion = if let Some(refusal) = refusal.clone() {
                            self.store
                                .complete_refused_turn_run_with_citations_and_append_event(
                                    turn.id,
                                    lease_token,
                                    expected_steer_revision,
                                    Utc::now(),
                                    &output,
                                    &citations,
                                    total_usage,
                                    refusal,
                                )
                                .await
                        } else {
                            self.store
                                .complete_turn_run_with_citations_and_append_event(
                                    turn.id,
                                    lease_token,
                                    expected_steer_revision,
                                    Utc::now(),
                                    &output,
                                    &citations,
                                    total_usage,
                                    stop_reason,
                                )
                                .await
                        };
                        match completion {
                            Ok(Some(resolution)) => match resolution.outcome {
                                CompleteTurnRunOutcome::Completed(_)
                                | CompleteTurnRunOutcome::Existing(_) => {
                                    if let Some(event) = resolution.terminal_event {
                                        self.publish(turn.chat_id, event);
                                    }
                                    // The cache counters are the only way to
                                    // see prompt caching work or silently stop:
                                    // a miss never errors, it just costs more.
                                    // Logged with the durable field names so the
                                    // line is greppable across turns.
                                    eprintln!(
                                        "openwave: turn {} resolved usage input_tokens={} output_tokens={} cache_read_input_tokens={} cache_creation_input_tokens={}",
                                        turn.id,
                                        total_usage.input_tokens,
                                        total_usage.output_tokens,
                                        total_usage.cache_read_input_tokens,
                                        total_usage.cache_creation_input_tokens,
                                    );
                                    return Ok(TurnWorkerOutcome::Completed(turn.id));
                                }
                                CompleteTurnRunOutcome::SteerPending(_)
                                | CompleteTurnRunOutcome::OutputSuperseded(_) => {
                                    break true;
                                }
                                CompleteTurnRunOutcome::ChildrenOutstanding {
                                    child_run_ids,
                                    ..
                                } => {
                                    continuation_instruction = Some(format!(
                                        "You cannot finish yet because background agents are still unsettled. Call wait_for_agents exactly once with this complete agent_ids list, preserving this order: {}",
                                        serde_json::to_string(&child_run_ids)?
                                    ));
                                    break true;
                                }
                            },
                            Ok(None) => {
                                // A concurrent durable admission can advance
                                // `updated_at` after this completion timestamp
                                // without changing the generation. Re-read the
                                // exact lease before deciding whether this was
                                // cancellation, lease loss, or a retryable CAS
                                // miss. Keep the original steer revision fence:
                                // if the steer was applied concurrently, the
                                // retry must supersede this output.
                                match self.live_turn_state_retry(&turn, lease_token).await {
                                    LiveTurnState::Running => tokio::task::yield_now().await,
                                    LiveTurnState::Cancelling => break false,
                                    LiveTurnState::Lost => {
                                        return Ok(TurnWorkerOutcome::LeaseLost(turn.id));
                                    }
                                }
                            }
                            Err(error) => {
                                self.retry_after("completion", turn.id, &error).await;
                                match self
                                    .resolution_state_retry(
                                        &turn,
                                        lease_token,
                                        TerminalIdentity::Completed {
                                            output: &output,
                                            citations: &citations,
                                            event: &terminal_event,
                                        },
                                    )
                                    .await
                                {
                                    ResolutionState::Retry => {}
                                    ResolutionState::Resolved(event) => {
                                        self.publish(turn.chat_id, *event);
                                        return Ok(TurnWorkerOutcome::Completed(turn.id));
                                    }
                                    ResolutionState::Cancelling => break false,
                                    ResolutionState::Lost => {
                                        return Ok(TurnWorkerOutcome::LeaseLost(turn.id));
                                    }
                                }
                            }
                        }
                    };
                    if continue_after_steer {
                        if remaining_steps == 0 {
                            drop(active);
                            return self
                                .record_failure(
                                    &turn,
                                    lease_token,
                                    total_model_steps,
                                    total_usage,
                                    "max_steps_exceeded",
                                    "max steps per turn exceeded while applying durable steering",
                                )
                                .await;
                        }
                        // Completion must stop the normal heartbeat to avoid
                        // racing its own CAS. Once completion chooses a
                        // nonterminal continuation, resume heartbeats while the
                        // journal append retries so a transient store outage
                        // cannot consume the accepted steer's lease.
                        let mut continuation_heartbeat = AbortOnDrop(tokio::spawn(
                            self.clone()
                                .heartbeat_lease(turn.clone(), lease_token, cancel.clone()),
                        ));
                        match self
                            .append_event(
                                &turn,
                                lease_token,
                                ordinal,
                                &AgentEvent::StreamInterrupted,
                            )
                            .await?
                        {
                            EventAppend::Committed => {
                                ordinal = ordinal.checked_add(1).ok_or_else(|| {
                                    AgentError::msg(format!(
                                        "turn {} event ordinal exhausted",
                                        turn.id
                                    ))
                                })?;
                            }
                            EventAppend::Cancelling => {
                                cancel.cancel();
                                drop(active);
                                return self
                                    .acknowledge_cancellation(&turn, lease_token, total_usage)
                                    .await;
                            }
                            EventAppend::LeaseLost => {
                                return Ok(TurnWorkerOutcome::LeaseLost(turn.id));
                            }
                        }
                        continuation_heartbeat.abort_and_wait().await;
                        continue;
                    }
                    drop(active);
                    return self
                        .acknowledge_cancellation(&turn, lease_token, total_usage)
                        .await;
                }
                Ok(AgentTurnOutcome::ClientToolCall {
                    request,
                    usage,
                    steer_revision,
                    model_steps,
                }) => {
                    if model_steps == 0 || model_steps > remaining_steps {
                        return Err(AgentError::msg(format!(
                            "turn {} returned an invalid model-step count {model_steps}",
                            turn.id
                        )));
                    }
                    remaining_steps -= model_steps;
                    total_model_steps = checked_model_step_sum(total_model_steps, model_steps)?;
                    total_usage = match checked_usage_sum(total_usage, usage) {
                        Ok(total) => total,
                        Err(_) => {
                            return self
                                .record_failure(
                                    &turn,
                                    lease_token,
                                    total_model_steps,
                                    total_usage,
                                    "usage_overflow",
                                    "provider usage exceeded the supported turn total",
                                )
                                .await;
                        }
                    };
                    // Parking on the last budgeted step is fine: the resuming
                    // segment arrives with zero steps and runs the wrap-up,
                    // which reads the client tool's result (#1181).
                    if !client_checkpoint_is_valid(
                        surface.tools.as_ref(),
                        turn.chat_id,
                        turn.id,
                        &request,
                    ) {
                        drop(active);
                        return self
                            .record_failure(
                                &turn,
                                lease_token,
                                total_model_steps,
                                total_usage,
                                "invalid_client_tool_call",
                                "agent returned an invalid client tool checkpoint",
                            )
                            .await;
                    }
                    checkpoint_usage = match checked_usage_sum(checkpoint_usage, usage) {
                        Ok(total) => total,
                        Err(_) => {
                            return self
                                .record_failure(
                                    &turn,
                                    lease_token,
                                    total_model_steps,
                                    total_usage,
                                    "usage_overflow",
                                    "provider usage exceeded the supported checkpoint total",
                                )
                                .await;
                        }
                    };
                    checkpoint_steps =
                        checkpoint_steps.checked_add(model_steps).ok_or_else(|| {
                            AgentError::msg(format!(
                                "turn {} checkpoint model-step count overflowed",
                                turn.id
                            ))
                        })?;
                    let progress = TurnCheckpointProgress {
                        model_steps: i32::try_from(checkpoint_steps).map_err(|_| {
                            AgentError::msg(format!(
                                "turn {} checkpoint model-step count is too large",
                                turn.id
                            ))
                        })?,
                        usage: checkpoint_usage,
                    };
                    let mut checkpoint_heartbeat = AbortOnDrop(tokio::spawn(
                        self.clone()
                            .heartbeat_lease(turn.clone(), lease_token, cancel.clone()),
                    ));
                    loop {
                        let park_result = tokio::select! {
                            result = self.store.park_turn_for_client_tool_call(
                                turn.id,
                                lease_token,
                                steer_revision,
                                progress,
                                Utc::now(),
                                &request,
                            ) => result,
                            result = &mut checkpoint_heartbeat.0 => {
                                match result {
                                    Ok(HeartbeatOutcome::Cancelling) => {
                                        drop(active);
                                        return self
                                            .acknowledge_cancellation(
                                                &turn,
                                                lease_token,
                                                total_usage,
                                            )
                                            .await;
                                    }
                                    Ok(HeartbeatOutcome::LeaseLost) | Err(_) => {
                                        return Ok(TurnWorkerOutcome::LeaseLost(turn.id));
                                    }
                                }
                            }
                        };
                        match park_result {
                            Ok(Some(ParkTurnForClientCallOutcome::Parked {
                                renderer_event,
                                ..
                            }))
                            | Ok(Some(ParkTurnForClientCallOutcome::Existing {
                                renderer_event,
                                ..
                            })) => {
                                if let Some(event) = renderer_event {
                                    self.publish(turn.chat_id, event);
                                }
                                checkpoint_heartbeat.abort_and_wait().await;
                                return Ok(TurnWorkerOutcome::WaitingForClient(turn.id));
                            }
                            Ok(Some(ParkTurnForClientCallOutcome::SteerPending(_)))
                            | Ok(Some(ParkTurnForClientCallOutcome::OutputSuperseded(_))) => {
                                break;
                            }
                            Ok(Some(ParkTurnForClientCallOutcome::IdentityConflict)) => {
                                checkpoint_heartbeat.abort_and_wait().await;
                                return self
                                    .record_failure(
                                        &turn,
                                        lease_token,
                                        total_model_steps,
                                        total_usage,
                                        "client_tool_identity_conflict",
                                        "client tool call identity conflicts with its durable receipt",
                                    )
                                    .await;
                            }
                            Ok(None) => {
                                match self.live_turn_state_retry(&turn, lease_token).await {
                                    LiveTurnState::Running => tokio::task::yield_now().await,
                                    LiveTurnState::Cancelling => {
                                        checkpoint_heartbeat.abort_and_wait().await;
                                        drop(active);
                                        return self
                                            .acknowledge_cancellation(
                                                &turn,
                                                lease_token,
                                                total_usage,
                                            )
                                            .await;
                                    }
                                    LiveTurnState::Lost => {
                                        checkpoint_heartbeat.abort_and_wait().await;
                                        return Ok(TurnWorkerOutcome::LeaseLost(turn.id));
                                    }
                                }
                            }
                            Err(error) => {
                                self.retry_after("client checkpoint", turn.id, &error).await;
                            }
                        }
                    }
                    checkpoint_heartbeat.abort_and_wait().await;
                    if remaining_steps == 0 {
                        drop(active);
                        return self
                            .record_failure(
                                &turn,
                                lease_token,
                                total_model_steps,
                                total_usage,
                                "max_steps_exceeded",
                                "max steps per turn exceeded while applying durable steering",
                            )
                            .await;
                    }
                    let mut continuation_heartbeat = AbortOnDrop(tokio::spawn(
                        self.clone()
                            .heartbeat_lease(turn.clone(), lease_token, cancel.clone()),
                    ));
                    match self
                        .append_event(&turn, lease_token, ordinal, &AgentEvent::StreamInterrupted)
                        .await?
                    {
                        EventAppend::Committed => {
                            ordinal = ordinal.checked_add(1).ok_or_else(|| {
                                AgentError::msg(format!("turn {} event ordinal exhausted", turn.id))
                            })?;
                        }
                        EventAppend::Cancelling => {
                            cancel.cancel();
                            drop(active);
                            return self
                                .acknowledge_cancellation(&turn, lease_token, total_usage)
                                .await;
                        }
                        EventAppend::LeaseLost => {
                            return Ok(TurnWorkerOutcome::LeaseLost(turn.id));
                        }
                    }
                    continuation_heartbeat.abort_and_wait().await;
                    continue;
                }
                Ok(AgentTurnOutcome::SandboxAgentSpawn {
                    request,
                    remaining_requests,
                    usage,
                    steer_revision,
                    model_steps,
                }) => {
                    if (model_steps == 0 && !had_pending_spawns) || model_steps > remaining_steps {
                        return Err(AgentError::msg(format!(
                            "turn {} returned an invalid model-step count {model_steps}",
                            turn.id
                        )));
                    }
                    remaining_steps -= model_steps;
                    pending_sandbox_spawns = remaining_requests;
                    pending_sandbox_spawn_steer_revision = Some(steer_revision);
                    total_model_steps = checked_model_step_sum(total_model_steps, model_steps)?;
                    total_usage = match checked_usage_sum(total_usage, usage) {
                        Ok(total) => total,
                        Err(_) => {
                            return self
                                .record_failure(
                                    &turn,
                                    lease_token,
                                    total_model_steps,
                                    total_usage,
                                    "usage_overflow",
                                    "provider usage exceeded the supported turn total",
                                )
                                .await;
                        }
                    };
                    // Parking on the last budgeted step is fine: the resuming
                    // segment arrives with zero steps and runs the wrap-up,
                    // which reads the delegation's result (#1181).
                    if !sandbox_spawn_checkpoint_is_valid(surface.tools.as_ref(), &request) {
                        drop(active);
                        return self
                            .record_failure(
                                &turn,
                                lease_token,
                                total_model_steps,
                                total_usage,
                                "invalid_sandbox_agent_spawn",
                                "agent returned an invalid sandbox delegation checkpoint",
                            )
                            .await;
                    }
                    checkpoint_usage = match checked_usage_sum(checkpoint_usage, usage) {
                        Ok(total) => total,
                        Err(_) => {
                            return self
                                .record_failure(
                                    &turn,
                                    lease_token,
                                    total_model_steps,
                                    total_usage,
                                    "usage_overflow",
                                    "provider usage exceeded the supported checkpoint total",
                                )
                                .await;
                        }
                    };
                    checkpoint_steps =
                        checkpoint_steps.checked_add(model_steps).ok_or_else(|| {
                            AgentError::msg(format!(
                                "turn {} checkpoint model-step count overflowed",
                                turn.id
                            ))
                        })?;
                    let progress = TurnCheckpointProgress {
                        model_steps: i32::try_from(checkpoint_steps).map_err(|_| {
                            AgentError::msg(format!(
                                "turn {} checkpoint model-step count is too large",
                                turn.id
                            ))
                        })?,
                        usage: checkpoint_usage,
                    };
                    let checkpoint = SandboxSpawnCheckpointRequest {
                        origin_turn_id: turn.id,
                        lease_token,
                        expected_steer_revision: steer_revision,
                        call_id: request.call_id,
                        provider_id: request.provider_id.clone(),
                        arguments: request.arguments.clone(),
                        approval_gated: request.approval_gated,
                        result: serde_json::to_string(&openwave_core::SpawnSandboxAgentResult {
                            agent_id: request.child_run_id,
                        })?,
                        event_ordinal: ordinal,
                        progress,
                        remaining_requests: pending_sandbox_spawns.clone(),
                        max_active_background_agents:
                            crate::routes::read_max_active_background_agents(&*self.store).await?,
                        execution_location: self.config.sandbox_spawn_execution_location,
                    };
                    let mut checkpoint_heartbeat = AbortOnDrop(tokio::spawn(
                        self.clone()
                            .heartbeat_lease(turn.clone(), lease_token, cancel.clone()),
                    ));
                    loop {
                        let park_result = tokio::select! {
                            result = self.store.checkpoint_sandbox_spawn(&checkpoint, Utc::now()) => result,
                            result = &mut checkpoint_heartbeat.0 => {
                                match result {
                                    Ok(HeartbeatOutcome::Cancelling) => {
                                        drop(active);
                                        return self
                                            .acknowledge_cancellation(
                                                &turn,
                                                lease_token,
                                                total_usage,
                                            )
                                            .await;
                                    }
                                    Ok(HeartbeatOutcome::LeaseLost) | Err(_) => {
                                        return Ok(TurnWorkerOutcome::LeaseLost(turn.id));
                                    }
                                }
                            }
                        };
                        match park_result {
                            Ok(Some(CheckpointSandboxSpawnOutcome::Checkpointed {
                                event, ..
                            })) => {
                                checkpoint_heartbeat.abort_and_wait().await;
                                self.publish(turn.chat_id, event);
                                self.sandbox_agent_wake.notify_one();
                                self.wake.notify_one();
                                return Ok(TurnWorkerOutcome::Resuming(turn.id));
                            }
                            Ok(Some(CheckpointSandboxSpawnOutcome::Existing { event, .. })) => {
                                checkpoint_heartbeat.abort_and_wait().await;
                                // Exact recovery can be the first successful
                                // response observed after an ambiguous commit.
                                // Re-publishing the same durable sequence is
                                // safe for cursor-deduplicating clients.
                                self.publish(turn.chat_id, event);
                                self.sandbox_agent_wake.notify_one();
                                self.wake.notify_one();
                                return Ok(TurnWorkerOutcome::Resuming(turn.id));
                            }
                            Ok(Some(CheckpointSandboxSpawnOutcome::SteerPending(_)))
                            | Ok(Some(CheckpointSandboxSpawnOutcome::OutputSuperseded(_))) => {
                                // The approval gate may already have committed
                                // a decision for this exact call. Keep that
                                // admitted head with its original siblings;
                                // the next loop applies the steer, replays the
                                // head without a second approval, and gates the
                                // still-ungated tail normally.
                                pending_sandbox_spawns.insert(0, request);
                                break;
                            }
                            Ok(Some(CheckpointSandboxSpawnOutcome::AtCapacity)) => {
                                continuation_instruction = Some(
                                    format!(
                                        "The per-chat background-agent cap is {} active agents. This spawn was not run. Call wait_for_agents with previously returned agent IDs, then try again after one finishes.",
                                        checkpoint.max_active_background_agents
                                    ),
                                );
                                pending_sandbox_spawns.clear();
                                pending_sandbox_spawn_steer_revision = None;
                                break;
                            }
                            Ok(Some(
                                CheckpointSandboxSpawnOutcome::DelegatedResourceUnavailable,
                            )) => {
                                continuation_instruction = Some(
                                    "That delegated file is no longer connected to this conversation. Continue without it or choose a currently connected file."
                                        .into(),
                                );
                                break;
                            }
                            Ok(Some(CheckpointSandboxSpawnOutcome::IdentityConflict))
                            | Ok(Some(CheckpointSandboxSpawnOutcome::ParentUnavailable)) => {
                                checkpoint_heartbeat.abort_and_wait().await;
                                return self
                                    .record_failure(
                                        &turn,
                                        lease_token,
                                        total_model_steps,
                                        total_usage,
                                        "sandbox_agent_spawn_identity_conflict",
                                        "sandbox delegation conflicts with its durable receipt",
                                    )
                                    .await;
                            }
                            Ok(Some(CheckpointSandboxSpawnOutcome::LeaseLost)) => {
                                match self.live_turn_state_retry(&turn, lease_token).await {
                                    LiveTurnState::Running => tokio::task::yield_now().await,
                                    LiveTurnState::Cancelling => {
                                        checkpoint_heartbeat.abort_and_wait().await;
                                        drop(active);
                                        return self
                                            .acknowledge_cancellation(
                                                &turn,
                                                lease_token,
                                                total_usage,
                                            )
                                            .await;
                                    }
                                    LiveTurnState::Lost => {
                                        checkpoint_heartbeat.abort_and_wait().await;
                                        return Ok(TurnWorkerOutcome::LeaseLost(turn.id));
                                    }
                                }
                            }
                            Ok(None) => {
                                match self.live_turn_state_retry(&turn, lease_token).await {
                                    LiveTurnState::Running => tokio::task::yield_now().await,
                                    LiveTurnState::Cancelling => {
                                        checkpoint_heartbeat.abort_and_wait().await;
                                        drop(active);
                                        return self
                                            .acknowledge_cancellation(
                                                &turn,
                                                lease_token,
                                                total_usage,
                                            )
                                            .await;
                                    }
                                    LiveTurnState::Lost => {
                                        checkpoint_heartbeat.abort_and_wait().await;
                                        return Ok(TurnWorkerOutcome::LeaseLost(turn.id));
                                    }
                                }
                            }
                            Err(error) => {
                                self.retry_after("sandbox delegation checkpoint", turn.id, &error)
                                    .await;
                            }
                        }
                    }
                    checkpoint_heartbeat.abort_and_wait().await;
                    if remaining_steps == 0 {
                        drop(active);
                        return self
                            .record_failure(
                                &turn,
                                lease_token,
                                total_model_steps,
                                total_usage,
                                "max_steps_exceeded",
                                "max steps per turn exceeded while applying durable steering",
                            )
                            .await;
                    }
                    let mut continuation_heartbeat = AbortOnDrop(tokio::spawn(
                        self.clone()
                            .heartbeat_lease(turn.clone(), lease_token, cancel.clone()),
                    ));
                    match self
                        .append_event(&turn, lease_token, ordinal, &AgentEvent::StreamInterrupted)
                        .await?
                    {
                        EventAppend::Committed => {
                            ordinal = ordinal.checked_add(1).ok_or_else(|| {
                                AgentError::msg(format!("turn {} event ordinal exhausted", turn.id))
                            })?;
                        }
                        EventAppend::Cancelling => {
                            cancel.cancel();
                            drop(active);
                            return self
                                .acknowledge_cancellation(&turn, lease_token, total_usage)
                                .await;
                        }
                        EventAppend::LeaseLost => {
                            return Ok(TurnWorkerOutcome::LeaseLost(turn.id));
                        }
                    }
                    continuation_heartbeat.abort_and_wait().await;
                    continue;
                }
                Ok(AgentTurnOutcome::WaitForAgents {
                    request,
                    usage,
                    steer_revision,
                    model_steps,
                }) => {
                    if model_steps == 0 || model_steps > remaining_steps {
                        return Err(AgentError::msg(format!(
                            "turn {} returned an invalid model-step count {model_steps}",
                            turn.id
                        )));
                    }
                    remaining_steps -= model_steps;
                    total_model_steps = checked_model_step_sum(total_model_steps, model_steps)?;
                    total_usage = match checked_usage_sum(total_usage, usage) {
                        Ok(total) => total,
                        Err(_) => {
                            return self
                                .record_failure(
                                    &turn,
                                    lease_token,
                                    total_model_steps,
                                    total_usage,
                                    "usage_overflow",
                                    "provider usage exceeded the supported turn total",
                                )
                                .await;
                        }
                    };
                    // Parking on the last budgeted step is fine: the resuming
                    // segment arrives with zero steps and runs the wrap-up,
                    // which reads the children's results (#1181).
                    if !agent_wait_checkpoint_is_valid(surface.tools.as_ref(), &request) {
                        drop(active);
                        return self
                            .record_failure(
                                &turn,
                                lease_token,
                                total_model_steps,
                                total_usage,
                                "invalid_agent_wait",
                                "agent returned an invalid background-agent wait checkpoint",
                            )
                            .await;
                    }
                    checkpoint_usage = match checked_usage_sum(checkpoint_usage, usage) {
                        Ok(total) => total,
                        Err(_) => {
                            return self
                                .record_failure(
                                    &turn,
                                    lease_token,
                                    total_model_steps,
                                    total_usage,
                                    "usage_overflow",
                                    "provider usage exceeded the supported checkpoint total",
                                )
                                .await;
                        }
                    };
                    checkpoint_steps =
                        checkpoint_steps.checked_add(model_steps).ok_or_else(|| {
                            AgentError::msg(format!(
                                "turn {} checkpoint model-step count overflowed",
                                turn.id
                            ))
                        })?;
                    let progress = TurnCheckpointProgress {
                        model_steps: i32::try_from(checkpoint_steps).map_err(|_| {
                            AgentError::msg(format!(
                                "turn {} checkpoint model-step count is too large",
                                turn.id
                            ))
                        })?,
                        usage: checkpoint_usage,
                    };
                    let checkpoint = AgentRunWaitSetCheckpointRequest {
                        call_id: request.call_id,
                        origin_turn_id: turn.id,
                        child_run_ids: request.child_run_ids.clone(),
                        condition: AgentRunWaitCondition::All,
                        lease_token,
                        expected_steer_revision: steer_revision,
                        provider_id: request.provider_id.clone(),
                        arguments: request.arguments.clone(),
                        event_ordinal: ordinal,
                        progress,
                    };
                    let mut checkpoint_heartbeat = AbortOnDrop(tokio::spawn(
                        self.clone()
                            .heartbeat_lease(turn.clone(), lease_token, cancel.clone()),
                    ));
                    loop {
                        let park_result = tokio::select! {
                            result = self.store.park_turn_for_agent_run_wait_set(&checkpoint, Utc::now()) => result,
                            result = &mut checkpoint_heartbeat.0 => {
                                match result {
                                    Ok(HeartbeatOutcome::Cancelling) => {
                                        drop(active);
                                        return self
                                            .acknowledge_cancellation(
                                                &turn,
                                                lease_token,
                                                total_usage,
                                            )
                                            .await;
                                    }
                                    Ok(HeartbeatOutcome::LeaseLost) | Err(_) => {
                                        return Ok(TurnWorkerOutcome::LeaseLost(turn.id));
                                    }
                                }
                            }
                        };
                        match park_result {
                            Ok(Some(ParkTurnForAgentRunWaitSetOutcome::Parked { .. }))
                            | Ok(Some(ParkTurnForAgentRunWaitSetOutcome::Existing { .. })) => {
                                checkpoint_heartbeat.abort_and_wait().await;
                                // This also drives the durable ready-set scan
                                // when every child completed before the park.
                                self.sandbox_agent_wake.notify_one();
                                return Ok(TurnWorkerOutcome::WaitingForAgentRun(turn.id));
                            }
                            Ok(Some(ParkTurnForAgentRunWaitSetOutcome::SteerPending(_)))
                            | Ok(Some(ParkTurnForAgentRunWaitSetOutcome::OutputSuperseded(_))) => {
                                break;
                            }
                            Ok(Some(ParkTurnForAgentRunWaitSetOutcome::IdentityConflict)) => {
                                continuation_instruction = Some(
                                    "That background-agent wait was invalid. Use only unique agent IDs returned by spawn_sandbox_agent in this turn, and include every unsettled child before finishing."
                                        .into(),
                                );
                                break;
                            }
                            Ok(None) => {
                                match self.live_turn_state_retry(&turn, lease_token).await {
                                    LiveTurnState::Running => tokio::task::yield_now().await,
                                    LiveTurnState::Cancelling => {
                                        checkpoint_heartbeat.abort_and_wait().await;
                                        drop(active);
                                        return self
                                            .acknowledge_cancellation(
                                                &turn,
                                                lease_token,
                                                total_usage,
                                            )
                                            .await;
                                    }
                                    LiveTurnState::Lost => {
                                        checkpoint_heartbeat.abort_and_wait().await;
                                        return Ok(TurnWorkerOutcome::LeaseLost(turn.id));
                                    }
                                }
                            }
                            Err(error) => {
                                self.retry_after(
                                    "background-agent wait checkpoint",
                                    turn.id,
                                    &error,
                                )
                                .await;
                            }
                        }
                    }
                    checkpoint_heartbeat.abort_and_wait().await;
                    let mut continuation_heartbeat = AbortOnDrop(tokio::spawn(
                        self.clone()
                            .heartbeat_lease(turn.clone(), lease_token, cancel.clone()),
                    ));
                    match self
                        .append_event(&turn, lease_token, ordinal, &AgentEvent::StreamInterrupted)
                        .await?
                    {
                        EventAppend::Committed => {
                            ordinal = ordinal.checked_add(1).ok_or_else(|| {
                                AgentError::msg(format!("turn {} event ordinal exhausted", turn.id))
                            })?;
                        }
                        EventAppend::Cancelling => {
                            cancel.cancel();
                            drop(active);
                            return self
                                .acknowledge_cancellation(&turn, lease_token, total_usage)
                                .await;
                        }
                        EventAppend::LeaseLost => {
                            return Ok(TurnWorkerOutcome::LeaseLost(turn.id));
                        }
                    }
                    continuation_heartbeat.abort_and_wait().await;
                    continue;
                }
                Ok(AgentTurnOutcome::Cancelled {
                    output,
                    citations,
                    usage,
                    ..
                }) => {
                    match checked_usage_sum(total_usage, usage) {
                        Ok(total) => total_usage = total,
                        Err(error) => eprintln!(
                            "openwave: turn {} cancellation usage overflowed; acknowledging the durable baseline: {error}",
                            turn.id
                        ),
                    }
                    drop(active);
                    return self
                        .acknowledge_cancellation_with_output(
                            &turn,
                            lease_token,
                            total_usage,
                            output.map(|message| (message, citations)),
                        )
                        .await;
                }
                Ok(AgentTurnOutcome::Failed {
                    error,
                    retry_after,
                    usage,
                    model_steps,
                }) => {
                    // A wrap-up-only attempt reports zero steps when it fails,
                    // exactly as it does when it completes.
                    if model_steps > remaining_steps || (model_steps == 0 && remaining_steps != 0) {
                        return Err(AgentError::msg(format!(
                            "turn {} returned an invalid failed model-step count {model_steps}",
                            turn.id
                        )));
                    }
                    total_model_steps = checked_model_step_sum(total_model_steps, model_steps)?;
                    total_usage = match checked_usage_sum(total_usage, usage) {
                        Ok(total) => total,
                        Err(_) => {
                            return self
                                .record_failure(
                                    &turn,
                                    lease_token,
                                    total_model_steps,
                                    total_usage,
                                    "usage_overflow",
                                    "provider usage exceeded the supported turn total",
                                )
                                .await;
                        }
                    };
                    drop(active);
                    return self
                        .record_classified_failure(
                            &turn,
                            lease_token,
                            total_model_steps,
                            total_usage,
                            &error.kind,
                            &error.message,
                            retry_after,
                        )
                        .await;
                }
                Err(error) => {
                    if self.is_cancelling_retry(&turn, lease_token).await {
                        drop(active);
                        return self
                            .acknowledge_cancellation(&turn, lease_token, total_usage)
                            .await;
                    }
                    return self
                        .record_classified_failure(
                            &turn,
                            lease_token,
                            total_model_steps,
                            total_usage,
                            error.kind(),
                            &error.to_string(),
                            error.retry_after(),
                        )
                        .await;
                }
            }
        }
    }

    async fn append_event(
        &self,
        turn: &TurnRun,
        lease_token: uuid::Uuid,
        ordinal: i32,
        event: &AgentEvent,
    ) -> Result<EventAppend> {
        loop {
            match self
                .store
                .append_turn_event(
                    turn.chat_id,
                    turn.id,
                    lease_token,
                    ordinal,
                    Utc::now(),
                    event,
                )
                .await
            {
                Ok(Some(seq)) => {
                    self.publish(
                        turn.chat_id,
                        SequencedEvent {
                            seq,
                            event: event.clone(),
                        },
                    );
                    return Ok(EventAppend::Committed);
                }
                Ok(None) => match self.lease_state_retry(turn, lease_token).await {
                    LeaseState::Running => continue,
                    LeaseState::Cancelling => return Ok(EventAppend::Cancelling),
                    LeaseState::Lost => return Ok(EventAppend::LeaseLost),
                },
                Err(error) => {
                    self.retry_after("event append", turn.id, &error).await;
                    match self.lease_state_retry(turn, lease_token).await {
                        LeaseState::Running => {}
                        LeaseState::Cancelling => return Ok(EventAppend::Cancelling),
                        LeaseState::Lost => return Ok(EventAppend::LeaseLost),
                    }
                }
            }
        }
    }

    async fn heartbeat_lease(
        self,
        turn: TurnRun,
        lease_token: uuid::Uuid,
        cancel: openwave_core::CancelToken,
    ) -> HeartbeatOutcome {
        let mut interval = tokio::time::interval(self.config.heartbeat);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        interval.tick().await;
        loop {
            interval.tick().await;
            match self.renew_lease(&turn, lease_token).await {
                LeaseState::Running => {}
                LeaseState::Cancelling => {
                    cancel.cancel();
                    return HeartbeatOutcome::Cancelling;
                }
                LeaseState::Lost => {
                    cancel.cancel();
                    return HeartbeatOutcome::LeaseLost;
                }
            }
        }
    }

    async fn renew_lease(&self, turn: &TurnRun, lease_token: uuid::Uuid) -> LeaseState {
        loop {
            let now = Utc::now();
            let Ok(lease) = chrono_duration(self.config.lease) else {
                return LeaseState::Lost;
            };
            match self
                .store
                .heartbeat_turn_run(turn.id, lease_token, now, now + lease)
                .await
            {
                Ok(true) => return LeaseState::Running,
                Ok(false) => return self.lease_state_retry(turn, lease_token).await,
                Err(error) => {
                    self.retry_after("heartbeat", turn.id, &error).await;
                    match self.lease_state_retry(turn, lease_token).await {
                        LeaseState::Running => {}
                        state => return state,
                    }
                }
            }
        }
    }

    async fn is_cancelling_retry(&self, turn: &TurnRun, lease_token: uuid::Uuid) -> bool {
        self.lease_state_retry(turn, lease_token).await == LeaseState::Cancelling
    }

    async fn lease_state_retry(&self, turn: &TurnRun, lease_token: uuid::Uuid) -> LeaseState {
        loop {
            match self.store.get_turn_run(turn.id).await {
                Ok(Some(current))
                    if current.lease_token == Some(lease_token)
                        && current.attempt_count == turn.attempt_count
                        && current
                            .lease_expires_at
                            .is_some_and(|expires_at| expires_at > Utc::now()) =>
                {
                    return match current.status {
                        TurnRunStatus::Running => LeaseState::Running,
                        TurnRunStatus::Cancelling => LeaseState::Cancelling,
                        _ => LeaseState::Lost,
                    };
                }
                Ok(_) => return LeaseState::Lost,
                Err(error) => {
                    self.retry_after("cancellation check", turn.id, &error)
                        .await
                }
            }
        }
    }

    async fn live_turn_state_retry(
        &self,
        turn: &TurnRun,
        lease_token: uuid::Uuid,
    ) -> LiveTurnState {
        loop {
            match self.store.get_turn_run(turn.id).await {
                Ok(Some(current))
                    if current.lease_token == Some(lease_token)
                        && current.attempt_count == turn.attempt_count
                        && current
                            .lease_expires_at
                            .is_some_and(|expires_at| expires_at > Utc::now()) =>
                {
                    return match current.status {
                        TurnRunStatus::Running => LiveTurnState::Running,
                        TurnRunStatus::Cancelling => LiveTurnState::Cancelling,
                        _ => LiveTurnState::Lost,
                    };
                }
                Ok(_) => return LiveTurnState::Lost,
                Err(error) => {
                    self.retry_after("turn generation fence read", turn.id, &error)
                        .await;
                }
            }
        }
    }

    async fn resolution_state_retry(
        &self,
        turn: &TurnRun,
        lease_token: uuid::Uuid,
        terminal: TerminalIdentity<'_>,
    ) -> ResolutionState {
        loop {
            match self.store.get_turn_run(turn.id).await {
                Ok(Some(current)) if current.attempt_count == turn.attempt_count => {
                    if current.status == terminal.status() {
                        if !terminal.matches_turn(&current) {
                            return ResolutionState::Lost;
                        }
                        let recovered = match &terminal {
                            TerminalIdentity::Completed {
                                output,
                                citations,
                                event,
                            } => {
                                self.store
                                    .recover_exact_completed_turn_event(
                                        turn.id,
                                        lease_token,
                                        output,
                                        citations,
                                        event,
                                    )
                                    .await
                            }
                            TerminalIdentity::Failed { .. }
                            | TerminalIdentity::Cancelled { .. } => {
                                self.store
                                    .recover_exact_turn_terminal_event(
                                        turn.id,
                                        lease_token,
                                        terminal.event(),
                                    )
                                    .await
                            }
                        };
                        return match recovered {
                            Ok(Some(event)) => ResolutionState::Resolved(Box::new(event)),
                            Ok(None) => ResolutionState::Lost,
                            Err(error) => {
                                self.retry_after("terminal recovery", turn.id, &error).await;
                                continue;
                            }
                        };
                    }
                    let exact_live_lease = current.lease_token == Some(lease_token)
                        && current
                            .lease_expires_at
                            .is_some_and(|expires_at| expires_at > Utc::now());
                    return match current.status {
                        TurnRunStatus::Running if exact_live_lease => ResolutionState::Retry,
                        TurnRunStatus::Cancelling if exact_live_lease => {
                            ResolutionState::Cancelling
                        }
                        _ => ResolutionState::Lost,
                    };
                }
                Ok(_) => return ResolutionState::Lost,
                Err(error) => {
                    self.retry_after("resolution state check", turn.id, &error)
                        .await;
                }
            }
        }
    }

    async fn acknowledge_cancellation(
        &self,
        turn: &TurnRun,
        lease_token: uuid::Uuid,
        usage: openwave_core::Usage,
    ) -> Result<TurnWorkerOutcome> {
        self.acknowledge_cancellation_with_output(turn, lease_token, usage, None)
            .await
    }

    /// Acknowledge a cancellation, committing any partial prose the cancelled
    /// agent loop carried out as the turn's durable output (#1182).
    async fn acknowledge_cancellation_with_output(
        &self,
        turn: &TurnRun,
        lease_token: uuid::Uuid,
        usage: openwave_core::Usage,
        output: Option<(
            openwave_core::Message,
            Vec<openwave_core::AssistantCitationInput>,
        )>,
    ) -> Result<TurnWorkerOutcome> {
        let terminal_event = AgentEvent::TurnCancelled { usage };
        loop {
            match self
                .store
                .finish_turn_cancellation_and_append_event(
                    turn.id,
                    lease_token,
                    Utc::now(),
                    usage,
                    output.as_ref().map(|(message, _)| message),
                    output
                        .as_ref()
                        .map(|(_, citations)| citations.as_slice())
                        .unwrap_or(&[]),
                )
                .await
            {
                Ok(Some(resolution)) => {
                    if let Some(event) = resolution.terminal_event {
                        self.publish(turn.chat_id, event);
                    }
                    return Ok(TurnWorkerOutcome::Cancelled(turn.id));
                }
                Ok(None) => return Ok(TurnWorkerOutcome::LeaseLost(turn.id)),
                Err(error) => {
                    self.retry_after("cancellation acknowledgement", turn.id, &error)
                        .await;
                    match self
                        .resolution_state_retry(
                            turn,
                            lease_token,
                            TerminalIdentity::Cancelled {
                                event: &terminal_event,
                            },
                        )
                        .await
                    {
                        ResolutionState::Retry | ResolutionState::Cancelling => {}
                        ResolutionState::Resolved(event) => {
                            self.publish(turn.chat_id, *event);
                            return Ok(TurnWorkerOutcome::Cancelled(turn.id));
                        }
                        ResolutionState::Lost => {
                            return Ok(TurnWorkerOutcome::LeaseLost(turn.id));
                        }
                    }
                }
            }
        }
    }

    async fn record_failure(
        &self,
        turn: &TurnRun,
        lease_token: uuid::Uuid,
        model_steps: i32,
        usage: openwave_core::Usage,
        code: &str,
        detail: &str,
    ) -> Result<TurnWorkerOutcome> {
        self.record_failure_with_retry(
            turn,
            lease_token,
            model_steps,
            usage,
            code,
            detail,
            TurnFailureRetry::Permanent,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn record_classified_failure(
        &self,
        turn: &TurnRun,
        lease_token: uuid::Uuid,
        model_steps: i32,
        usage: openwave_core::Usage,
        code: &str,
        detail: &str,
        retry_after: Option<Duration>,
    ) -> Result<TurnWorkerOutcome> {
        let retry = crate::event_projection::TurnFailureCategory::from_kind(code)
            .retries_may_succeed()
            .then(|| {
                self.config
                    .retry
                    .next_attempt_at(retry_attempt(turn), retry_after, Utc::now())
            })
            .flatten()
            .map_or(TurnFailureRetry::Permanent, TurnFailureRetry::RetryAt);
        self.record_failure_with_retry(turn, lease_token, model_steps, usage, code, detail, retry)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn record_failure_with_retry(
        &self,
        turn: &TurnRun,
        lease_token: uuid::Uuid,
        model_steps: i32,
        usage: openwave_core::Usage,
        code: &str,
        detail: &str,
        retry: TurnFailureRetry,
    ) -> Result<TurnWorkerOutcome> {
        let mut detail: String = detail
            .chars()
            .map(|character| {
                if character == '\0' {
                    '\u{fffd}'
                } else {
                    character
                }
            })
            .take(TurnRun::MAX_ERROR_DETAIL_LEN)
            .collect();
        if detail.is_empty() {
            detail.push_str("agent execution failed");
        }
        let terminal_event = AgentEvent::TurnFailed {
            error: openwave_core::AgentErrorInfo {
                kind: code.to_owned(),
                message: detail.clone(),
            },
        };
        loop {
            match self
                .store
                .record_turn_run_failure_and_append_event(
                    turn.id,
                    lease_token,
                    Utc::now(),
                    retry,
                    model_steps,
                    usage,
                    code,
                    Some(&detail),
                )
                .await
            {
                Ok(Some(resolution)) => {
                    if let Some(event) = resolution.terminal_event {
                        self.publish(turn.chat_id, event);
                    }
                    return Ok(match resolution.outcome {
                        RecordTurnFailureOutcome::Recorded(_)
                        | RecordTurnFailureOutcome::Existing(_) => {
                            TurnWorkerOutcome::Failed(turn.id)
                        }
                    });
                }
                Ok(None) => match self.live_turn_state_retry(turn, lease_token).await {
                    LiveTurnState::Running => tokio::task::yield_now().await,
                    LiveTurnState::Cancelling => {
                        return self
                            .acknowledge_cancellation(turn, lease_token, usage)
                            .await;
                    }
                    LiveTurnState::Lost => return Ok(TurnWorkerOutcome::LeaseLost(turn.id)),
                },
                Err(error) => {
                    self.retry_after("failure resolution", turn.id, &error)
                        .await;
                    if !matches!(retry, TurnFailureRetry::Permanent) {
                        // Retry the exact failure identity and timestamp. The
                        // immutable failure receipt is recoverable even after
                        // the first commit released this lease into retry_wait.
                        continue;
                    }
                    match self
                        .resolution_state_retry(
                            turn,
                            lease_token,
                            TerminalIdentity::Failed {
                                code,
                                detail: &detail,
                                event: &terminal_event,
                            },
                        )
                        .await
                    {
                        ResolutionState::Retry => {}
                        ResolutionState::Resolved(event) => {
                            self.publish(turn.chat_id, *event);
                            return Ok(TurnWorkerOutcome::Failed(turn.id));
                        }
                        ResolutionState::Cancelling => {
                            return self
                                .acknowledge_cancellation(turn, lease_token, usage)
                                .await;
                        }
                        ResolutionState::Lost => {
                            return Ok(TurnWorkerOutcome::LeaseLost(turn.id));
                        }
                    }
                }
            }
        }
    }

    async fn retry_after(&self, operation: &str, turn_id: TurnId, error: &AgentError) {
        eprintln!("openwave: turn {turn_id} {operation} failed; retrying exact request: {error}");
        tokio::time::sleep(self.config.failure_delay).await;
    }

    fn publish(&self, chat_id: openwave_core::ChatId, event: SequencedEvent) {
        let _ = self.events.sender(chat_id).send(event);
    }
}

/// One chat's runtime-only scratch directory under `root`.
///
/// The path is derived rather than stored, so the turn worker that journals a
/// scratch write and the transcript that labels it later agree without either
/// one persisting a host path.
pub(crate) fn private_chat_scratch_path(root: &Path, chat_id: openwave_core::ChatId) -> PathBuf {
    root.join(chat_id.to_string())
}

/// Derive and create one chat's runtime-only scratch under the private server
/// data directory. Product records and API responses never receive this path.
fn private_chat_scratch(
    root: &Path,
    chat_id: openwave_core::ChatId,
) -> std::io::Result<ToolScratch> {
    let mut root_builder = fs::DirBuilder::new();
    root_builder.recursive(true);
    #[cfg(unix)]
    root_builder.mode(0o700);
    root_builder.create(root)?;
    let root_meta = fs::symlink_metadata(root)?;
    if !root_meta.is_dir() || root_meta.file_type().is_symlink() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "private scratch root is not a regular directory",
        ));
    }
    #[cfg(unix)]
    fs::set_permissions(root, fs::Permissions::from_mode(0o700))?;
    let root_dir = Dir::open_ambient_dir(root, ambient_authority())?;
    #[cfg(unix)]
    {
        let opened = root_dir.dir_metadata()?;
        if std::os::unix::fs::MetadataExt::dev(&root_meta) != cap_std::fs::MetadataExt::dev(&opened)
            || std::os::unix::fs::MetadataExt::ino(&root_meta)
                != cap_std::fs::MetadataExt::ino(&opened)
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "private scratch root changed while it was being pinned",
            ));
        }
    }
    let chat_name = chat_id.to_string();
    match root_dir.symlink_metadata(&chat_name) {
        Ok(metadata) if !metadata.is_dir() || metadata.file_type().is_symlink() => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "private chat scratch is not a regular directory",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            #[cfg(unix)]
            {
                let mut builder = DirBuilder::new();
                builder.mode(0o700);
                root_dir.create_dir_with(&chat_name, &builder)?;
            }
            #[cfg(not(unix))]
            root_dir.create_dir(&chat_name)?;
        }
        Err(error) => return Err(error),
    }
    #[cfg(unix)]
    root_dir.set_permissions(&chat_name, cap_std::fs::Permissions::from_mode(0o700))?;
    let chat_dir = root_dir.open_dir(&chat_name)?;
    Ok(ToolScratch::from_dir(chat_dir))
}

fn checked_usage_sum(
    total: openwave_core::Usage,
    delta: openwave_core::Usage,
) -> Result<openwave_core::Usage> {
    total
        .checked_add(delta)
        .ok_or_else(|| AgentError::msg("provider usage exceeded the supported turn total"))
}

fn checked_model_step_sum(total: i32, delta: usize) -> Result<i32> {
    let delta = i32::try_from(delta)
        .map_err(|_| AgentError::msg("model-step delta exceeds the durable range"))?;
    total
        .checked_add(delta)
        .ok_or_else(|| AgentError::msg("model-step total exceeds the durable range"))
}

/// The turn's retry state, with its envelope measured from the first claim.
fn retry_attempt(turn: &TurnRun) -> RetryAttempt {
    RetryAttempt {
        id: *turn.id.as_uuid(),
        attempt_count: turn.attempt_count,
        max_attempts: turn.max_attempts,
        first_attempt_at: turn.started_at.unwrap_or(turn.created_at),
    }
}

fn chrono_duration(duration: Duration) -> Result<chrono::Duration> {
    chrono::Duration::from_std(duration)
        .map_err(|error| AgentError::msg(format!("invalid turn-worker duration: {error}")))
}

fn log_turn_result(result: std::result::Result<Result<TurnWorkerOutcome>, tokio::task::JoinError>) {
    match result {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => eprintln!("openwave: turn worker execution failed: {error}"),
        Err(error) => eprintln!("openwave: turn worker task stopped: {error}"),
    }
}

#[cfg(test)]
mod committed_event_drain_tests {
    use super::*;

    #[test]
    fn chat_only_surface_freezes_matching_empty_tools_and_prompt() {
        let mut registry = ToolRegistry::new();
        for name in [
            "exec",
            "search",
            openwave_core::WEB_SEARCH_TOOL,
            openwave_core::REQUEST_FOLDER_ACCESS_TOOL,
            openwave_core::SPAWN_SANDBOX_AGENT_TOOL,
            openwave_core::WAIT_FOR_AGENTS_TOOL,
            "mcp__documents__lookup",
        ] {
            registry.register_client(
                openwave_core::ToolSpec {
                    name: name.into(),
                    description: "registered capability".into(),
                    input_schema: serde_json::json!({"type": "object"}),
                },
                openwave_core::ApprovalClass::ReadOnly,
            );
        }
        let surface = freeze_foreground_turn_surface(
            Arc::new(registry),
            &AgentConfig {
                tools_supported: false,
                ..AgentConfig::default()
            },
        );

        assert!(surface.tools.specs_for_foreground(true).is_empty());
        assert_eq!(
            surface.agent_config.web_search,
            openwave_core::TurnWebSearch::Off
        );
        let prompt = surface.agent_config.system_prompt.as_deref().unwrap();
        assert_eq!(prompt, crate::foreground_prompt::compose(&[]));
        for unavailable in [
            "exec",
            "search",
            "delegat",
            "done",
            "request_folder_access",
            "mcp__",
        ] {
            assert!(
                !prompt.contains(unavailable),
                "chat-only foreground prompt advertised unavailable capability `{unavailable}`: {prompt}"
            );
        }
    }

    /// Host policy `TurnWebSearch::Off` must keep `web_search` out of the
    /// operating prompt even while the process-wide registry still holds the
    /// inert tool. Advertisement and prompt are the turn surface; the registry
    /// alone must not describe a capability the operator turned off.
    #[test]
    fn web_search_off_keeps_the_tool_out_of_the_turn_prompt() {
        let mut registry = ToolRegistry::new();
        registry.register_client(
            openwave_core::web_search_tool_spec(),
            openwave_core::ApprovalClass::Sensitive,
        );
        registry.register_client(
            openwave_core::ToolSpec {
                name: "exec".into(),
                description: "run a command".into(),
                input_schema: serde_json::json!({"type": "object"}),
            },
            openwave_core::ApprovalClass::Sensitive,
        );
        let surface = freeze_foreground_turn_surface_with_folders(
            Arc::new(registry),
            &AgentConfig::default(),
            &[],
            &[],
            &[],
            &openwave_core::NetworkPolicy::default(),
            crate::code_execution::DEFAULT_TIMEOUT_MS,
            false,
            None,
            None,
            false,
            openwave_core::TurnWebSearch::Off,
        );

        assert_eq!(
            surface.agent_config.web_search,
            openwave_core::TurnWebSearch::Off
        );
        let prompt = surface.agent_config.system_prompt.as_deref().unwrap();
        assert!(
            !prompt.contains(openwave_core::WEB_SEARCH_TOOL),
            "off turn still described web_search: {prompt}"
        );
        assert!(
            prompt.contains("exec"),
            "off must not empty the rest of the tool surface: {prompt}"
        );
    }

    #[test]
    fn mcp_refresh_keeps_prompt_and_tools_on_one_immutable_snapshot() {
        let mut original_registry = ToolRegistry::new();
        original_registry.register_validated_foreground_client(
            openwave_core::ask_user_questions_tool_spec(),
            openwave_core::ApprovalClass::ReadOnly,
            openwave_core::validate_ask_user_questions_arguments,
        );
        let original_tools = Arc::new(original_registry);
        let original =
            freeze_foreground_turn_surface(original_tools.clone(), &AgentConfig::default());

        let mut refreshed_registry = (*original_tools).clone();
        refreshed_registry.register_client(
            openwave_core::ToolSpec {
                name: "mcp__documents__lookup".into(),
                description: "untrusted remote description marker".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "credential_marker": {"default": "untrusted remote schema marker"}
                    }
                }),
            },
            openwave_core::ApprovalClass::Sensitive,
        );
        let refreshed =
            freeze_foreground_turn_surface(Arc::new(refreshed_registry), &AgentConfig::default());

        let original_names = original
            .tools
            .specs_for_foreground(true)
            .into_iter()
            .map(|spec| spec.name)
            .collect::<Vec<_>>();
        let refreshed_names = refreshed
            .tools
            .specs_for_foreground(true)
            .into_iter()
            .map(|spec| spec.name)
            .collect::<Vec<_>>();
        let original_prompt = original.agent_config.system_prompt.as_deref().unwrap();
        let refreshed_prompt = refreshed.agent_config.system_prompt.as_deref().unwrap();

        assert!(!original_names.iter().any(|name| name.starts_with("mcp__")));
        assert!(original_names
            .iter()
            .any(|name| name == openwave_core::ASK_USER_QUESTIONS_TOOL));
        assert!(!original_prompt.contains("## External MCP tools"));
        assert!(original_prompt.contains("## User clarification"));
        assert!(refreshed_names
            .iter()
            .any(|name| name == "mcp__documents__lookup"));
        assert!(refreshed_names
            .iter()
            .any(|name| name == openwave_core::ASK_USER_QUESTIONS_TOOL));
        assert!(refreshed_prompt.contains("## External MCP tools"));
        assert!(refreshed_prompt.contains("## User clarification"));
        for prompt in [original_prompt, refreshed_prompt] {
            assert!(!prompt.contains("mcp__documents__lookup"));
            assert!(!prompt.contains("untrusted remote description marker"));
            assert!(!prompt.contains("untrusted remote schema marker"));
        }
    }

    #[test]
    fn private_scratch_is_isolated_per_chat() {
        let root = tempfile::tempdir().unwrap();
        let first_chat = openwave_core::ChatId::new();
        let second_chat = openwave_core::ChatId::new();

        let _first = private_chat_scratch(root.path(), first_chat).unwrap();
        let _second = private_chat_scratch(root.path(), second_chat).unwrap();
        let first = root.path().join(first_chat.to_string());
        let second = root.path().join(second_chat.to_string());

        assert_ne!(first, second);
        assert_eq!(first.parent(), second.parent());
        assert!(first.is_dir());
        assert!(second.is_dir());

        #[cfg(unix)]
        {
            assert_eq!(
                fs::metadata(&first).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(&second).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn private_scratch_rejects_a_symlinked_chat_directory() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let chat_id = openwave_core::ChatId::new();
        symlink(outside.path(), root.path().join(chat_id.to_string())).unwrap();

        let error = private_chat_scratch(root.path(), chat_id).unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }

    #[cfg(unix)]
    #[test]
    fn private_scratch_rejects_a_symlinked_root() {
        use std::os::unix::fs::symlink;

        let parent = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = parent.path().join("scratch");
        symlink(outside.path(), &root).unwrap();

        let error = private_chat_scratch(&root, openwave_core::ChatId::new()).unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pinned_private_scratch_survives_path_replacement_without_escaping() {
        use openwave_core::{Tool, ToolCtx, WriteFile};
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let chat_id = openwave_core::ChatId::new();
        let original = root.path().join(chat_id.to_string());
        let moved = root.path().join("pinned");
        let scratch = private_chat_scratch(root.path(), chat_id).unwrap();
        fs::rename(&original, &moved).unwrap();
        symlink(outside.path(), &original).unwrap();
        let ctx = ToolCtx::with_private_scratch(chat_id, None, scratch);

        let output = WriteFile
            .execute(
                &ctx,
                serde_json::json!({"path": "note.txt", "content": "pinned"}),
            )
            .await
            .unwrap();

        assert!(!output.is_error);
        assert_eq!(
            fs::read_to_string(moved.join("note.txt")).unwrap(),
            "pinned"
        );
        assert!(!outside.path().join("note.txt").exists());
    }

    #[tokio::test]
    async fn lease_loss_drain_discards_pending_and_publishes_committed_events() {
        let events = EventBus::default();
        let chat_id = openwave_core::ChatId::new();
        let mut live = events.subscribe(chat_id);
        let (sender, mut receiver) = unbounded();
        sender
            .unbounded_send(ClaimedAgentEvent::Pending {
                ordinal: 2,
                event: AgentEvent::TextDelta {
                    text: "not committed".into(),
                },
            })
            .unwrap();
        let committed = SequencedEvent {
            seq: 7,
            event: AgentEvent::UserSteered {
                message_id: MessageId::new(),
                content: "already durable".into(),
            },
        };
        sender
            .unbounded_send(ClaimedAgentEvent::Committed {
                ordinal: 3,
                event: committed.clone(),
            })
            .unwrap();
        drop(sender);

        drain_committed_events(&events, chat_id, &mut receiver).await;

        assert_eq!(live.try_recv().unwrap(), committed);
        assert!(matches!(
            live.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
    }

    #[test]
    fn usage_sum_checks_every_component_without_wrapping() {
        let total = openwave_core::Usage {
            input_tokens: 1,
            output_tokens: 2,
            cache_read_input_tokens: 3,
            cache_creation_input_tokens: 4,
        };
        let delta = openwave_core::Usage {
            input_tokens: 5,
            output_tokens: 6,
            cache_read_input_tokens: 7,
            cache_creation_input_tokens: 8,
        };
        assert_eq!(
            checked_usage_sum(total, delta).unwrap(),
            openwave_core::Usage {
                input_tokens: 6,
                output_tokens: 8,
                cache_read_input_tokens: 10,
                cache_creation_input_tokens: 12,
            }
        );
        assert!(checked_usage_sum(
            openwave_core::Usage {
                input_tokens: u32::MAX,
                ..openwave_core::Usage::default()
            },
            openwave_core::Usage {
                input_tokens: 1,
                ..openwave_core::Usage::default()
            },
        )
        .is_err());
    }

    #[test]
    fn client_checkpoint_fence_rejects_invalid_payloads_before_parking() {
        let mut tools = ToolRegistry::new();
        tools.register_validated_client(
            openwave_core::request_folder_access_tool_spec(),
            openwave_core::ApprovalClass::ReadOnly,
            openwave_core::validate_request_folder_access_arguments,
        );
        let chat_id = openwave_core::ChatId::new();
        let turn_id = TurnId::new();
        let mut request = openwave_core::ClientToolCallRequest {
            id: openwave_core::CallId::new(),
            chat_id,
            turn_id,
            provider_id: "provider-call-1".into(),
            name: openwave_core::REQUEST_FOLDER_ACCESS_TOOL.into(),
            arguments: serde_json::json!({
                "reason": "Read the reports needed for this project",
                "requested_capabilities": ["read_files"],
                "folder_hint": "documents"
            }),
        };
        assert!(client_checkpoint_is_valid(
            &tools, chat_id, turn_id, &request
        ));

        request.arguments = serde_json::json!({
            "reason": "Read reports",
            "requested_capabilities": ["write_files"],
            "path": "/Users/example/Documents"
        });
        assert!(!client_checkpoint_is_valid(
            &tools, chat_id, turn_id, &request
        ));
    }

    #[test]
    fn sandbox_spawn_checkpoint_fence_requires_the_registered_foreground_contract() {
        let mut tools = ToolRegistry::new();
        tools.register_foreground_agent_orchestration();
        let call_id = openwave_core::CallId::new();
        let request = SandboxAgentSpawnRequest {
            call_id,
            provider_id: "provider-call".into(),
            child_run_id: openwave_core::AgentRunId::sandbox_for_spawn_call(call_id),
            task: "Research the error handling options.".into(),
            arguments: serde_json::json!({"task":"Research the error handling options."}),
            approval_gated: false,
        };
        assert!(sandbox_spawn_checkpoint_is_valid(&tools, &request));

        let invalid_task = SandboxAgentSpawnRequest {
            task: " ".into(),
            ..request.clone()
        };
        assert!(!sandbox_spawn_checkpoint_is_valid(&tools, &invalid_task));

        let forged_child = SandboxAgentSpawnRequest {
            child_run_id: openwave_core::AgentRunId::new(),
            ..request
        };
        assert!(!sandbox_spawn_checkpoint_is_valid(&tools, &forged_child));
    }
}
