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
    AcceptSandboxAgentRunAndParkTurnOutcome, Agent, AgentConfig, AgentError, AgentEvent,
    AgentTurnOutcome, ClaimedAgentEvent, CompleteTurnRunOutcome, MessageId,
    ParkTurnForClientCallOutcome, RecordTurnFailureOutcome, Result, SandboxAgentSpawnRequest,
    SequencedEvent, Store, ToolRegistry, ToolScratch, TurnCheckpointProgress, TurnFailureRetry,
    TurnId, TurnRun, TurnRunStatus, SPAWN_SANDBOX_AGENT_TOOL,
};
use tokio::sync::Notify;

use crate::approvals::ApprovalBroker;
use crate::bus::EventBus;
use crate::resolver::ProviderResolver;
use crate::state::TurnGuard;

#[derive(Debug, Clone, Copy)]
pub(crate) struct TurnWorkerConfig {
    pub(crate) lease: Duration,
    pub(crate) heartbeat: Duration,
    pub(crate) steer_poll: Duration,
    pub(crate) idle_min: Duration,
    pub(crate) idle_cap: Duration,
    pub(crate) failure_delay: Duration,
    pub(crate) max_concurrency: usize,
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
            max_concurrency: 4,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TurnWorkerOutcome {
    Completed(TurnId),
    WaitingForClient(TurnId),
    WaitingForAgentRun(TurnId),
    Cancelled(TurnId),
    Failed(TurnId),
    LeaseLost(TurnId),
}

#[derive(Clone)]
pub(crate) struct TurnWorker {
    store: Arc<dyn Store>,
    resolver: Arc<dyn ProviderResolver>,
    tools: Arc<ToolRegistry>,
    approvals: Arc<ApprovalBroker>,
    events: Arc<EventBus>,
    signals: Arc<TurnGuard>,
    wake: Arc<Notify>,
    sandbox_agent_wake: Arc<Notify>,
    agent_config: AgentConfig,
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
    Resolved(SequencedEvent),
    Cancelling,
    Lost,
}

enum TerminalIdentity<'a> {
    Completed {
        output_message_id: MessageId,
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
            Self::Completed {
                output_message_id, ..
            } => turn.output_message_id == Some(*output_message_id),
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
        Self {
            store,
            resolver,
            tools,
            approvals,
            events,
            signals,
            wake,
            sandbox_agent_wake,
            agent_config,
            private_scratch_root,
            config,
        }
    }

    pub(crate) async fn run(self) {
        let mut turns = tokio::task::JoinSet::new();
        let mut idle_delay = self.config.idle_min;
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
                self.config.failure_delay
            } else {
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

    async fn process(&self, turn: TurnRun, lease_token: uuid::Uuid) -> Result<TurnWorkerOutcome> {
        if turn.status != TurnRunStatus::Running || turn.lease_token != Some(lease_token) {
            return Err(AgentError::msg(format!(
                "claimed turn {} has an invalid execution identity",
                turn.id
            )));
        }
        let mut total_model_steps = turn.model_steps;
        // Intermediate tool/message effects are not yet lease-CAS fenced. Keep
        // execution explicitly single-attempt so a crash can fail conservatively
        // but can never replay a filesystem or external tool side effect.
        if turn.max_attempts != 1 {
            return self
                .record_failure(
                    &turn,
                    lease_token,
                    total_model_steps,
                    turn.usage,
                    "unsupported_retry_policy",
                    "turn execution requires max_attempts = 1",
                )
                .await;
        }
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
        if remaining_steps == 0 {
            return self
                .record_failure(
                    &turn,
                    lease_token,
                    total_model_steps,
                    turn.usage,
                    "max_steps_exceeded",
                    "max steps per turn were consumed before this lease segment",
                )
                .await;
        }
        let Some(chat) = self.store.get_chat(turn.chat_id).await? else {
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
            let mut config = self.agent_config.clone();
            config.model = turn.model.clone();
            config.max_steps = remaining_steps;
            config.tool_scratch = self.private_scratch_root.as_deref().and_then(|root| {
                match private_chat_scratch(root, chat.id) {
                    Ok(scratch) => Some(scratch),
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
            let agent = Agent::new(provider, self.tools.clone(), self.store.clone(), config)
                .with_approvals(self.approvals.clone())
                .with_cancel(cancel.clone())
                .with_steer(steer.clone())
                .with_durable_steer(lease_token)
                .with_foreground_sandbox_spawns();
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
            match self.renew_lease(&turn, lease_token).await {
                LeaseState::Running => {}
                LeaseState::Cancelling => {
                    let usage = match &drive_result {
                        Ok(AgentTurnOutcome::Completed { usage, .. })
                        | Ok(AgentTurnOutcome::Cancelled { usage, .. })
                        | Ok(AgentTurnOutcome::ClientToolCall { usage, .. })
                        | Ok(AgentTurnOutcome::SandboxAgentSpawn { usage, .. })
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
                    return self
                        .acknowledge_cancellation(&turn, lease_token, total_usage)
                        .await;
                }
                LeaseState::Lost => return Ok(TurnWorkerOutcome::LeaseLost(turn.id)),
            }

            match drive_result {
                Ok(AgentTurnOutcome::Completed {
                    output,
                    usage,
                    stop_reason,
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
                    let expected_steer_revision = steer_revision.ok_or_else(|| {
                        AgentError::msg(format!(
                            "turn {} completed without a durable generation fence",
                            turn.id
                        ))
                    })?;
                    let terminal_event = AgentEvent::TurnCompleted {
                        usage: total_usage,
                        stop_reason,
                    };
                    let continue_after_steer = loop {
                        match self
                            .store
                            .complete_turn_run_and_append_event(
                                turn.id,
                                lease_token,
                                expected_steer_revision,
                                Utc::now(),
                                &output,
                                total_usage,
                                stop_reason,
                            )
                            .await
                        {
                            Ok(Some(resolution)) => match resolution.outcome {
                                CompleteTurnRunOutcome::Completed(_)
                                | CompleteTurnRunOutcome::Existing(_) => {
                                    if let Some(event) = resolution.terminal_event {
                                        self.publish(turn.chat_id, event);
                                    }
                                    return Ok(TurnWorkerOutcome::Completed(turn.id));
                                }
                                CompleteTurnRunOutcome::SteerPending(_)
                                | CompleteTurnRunOutcome::OutputSuperseded(_) => {
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
                                            output_message_id: output.id,
                                            event: &terminal_event,
                                        },
                                    )
                                    .await
                                {
                                    ResolutionState::Retry => {}
                                    ResolutionState::Resolved(event) => {
                                        self.publish(turn.chat_id, event);
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
                    if remaining_steps == 0 {
                        drop(active);
                        return self
                            .record_failure(
                                &turn,
                                lease_token,
                                total_model_steps,
                                total_usage,
                                "max_steps_exceeded",
                                "max steps per turn exceeded before client tool execution",
                            )
                            .await;
                    }
                    if !client_checkpoint_is_valid(
                        self.tools.as_ref(),
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
                            Ok(Some(ParkTurnForClientCallOutcome::Parked { .. }))
                            | Ok(Some(ParkTurnForClientCallOutcome::Existing { .. })) => {
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
                    if remaining_steps == 0 {
                        drop(active);
                        return self
                            .record_failure(
                                &turn,
                                lease_token,
                                total_model_steps,
                                total_usage,
                                "max_steps_exceeded",
                                "max steps per turn exceeded before sandbox delegation",
                            )
                            .await;
                    }
                    if !sandbox_spawn_checkpoint_is_valid(self.tools.as_ref(), &request) {
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
                    let mut checkpoint_heartbeat = AbortOnDrop(tokio::spawn(
                        self.clone()
                            .heartbeat_lease(turn.clone(), lease_token, cancel.clone()),
                    ));
                    loop {
                        let park_result = tokio::select! {
                            result = self.store.accept_sandbox_agent_run_and_park_turn(
                                request.child_run_id,
                                turn.id,
                                request.call_id,
                                &request.task,
                                lease_token,
                                steer_revision,
                                progress,
                                Utc::now(),
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
                            Ok(Some(AcceptSandboxAgentRunAndParkTurnOutcome::Parked {
                                ..
                            }))
                            | Ok(Some(AcceptSandboxAgentRunAndParkTurnOutcome::Existing {
                                ..
                            })) => {
                                checkpoint_heartbeat.abort_and_wait().await;
                                // The child and parent wait state are now one
                                // durable transaction. This is intentionally
                                // only a latency hint: the sandbox worker's
                                // durable claim scan remains the correctness
                                // source if the notification is lost.
                                self.sandbox_agent_wake.notify_one();
                                return Ok(TurnWorkerOutcome::WaitingForAgentRun(turn.id));
                            }
                            Ok(Some(AcceptSandboxAgentRunAndParkTurnOutcome::SteerPending(_)))
                            | Ok(Some(
                                AcceptSandboxAgentRunAndParkTurnOutcome::OutputSuperseded(_),
                            )) => {
                                break;
                            }
                            Ok(Some(AcceptSandboxAgentRunAndParkTurnOutcome::IdentityConflict))
                            | Ok(Some(
                                AcceptSandboxAgentRunAndParkTurnOutcome::ParentUnavailable,
                            )) => {
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
                Ok(AgentTurnOutcome::Cancelled { usage, .. }) => {
                    match checked_usage_sum(total_usage, usage) {
                        Ok(total) => total_usage = total,
                        Err(error) => eprintln!(
                            "openwave: turn {} cancellation usage overflowed; acknowledging the durable baseline: {error}",
                            turn.id
                        ),
                    }
                    drop(active);
                    return self
                        .acknowledge_cancellation(&turn, lease_token, total_usage)
                        .await;
                }
                Ok(AgentTurnOutcome::Failed {
                    error,
                    usage,
                    model_steps,
                }) => {
                    if model_steps == 0 || model_steps > remaining_steps {
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
                        .record_failure(
                            &turn,
                            lease_token,
                            total_model_steps,
                            total_usage,
                            &error.kind,
                            &error.message,
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
                        .record_failure(
                            &turn,
                            lease_token,
                            total_model_steps,
                            total_usage,
                            "agent_error",
                            &error.to_string(),
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
                        return match self
                            .store
                            .recover_exact_turn_terminal_event(
                                turn.id,
                                lease_token,
                                terminal.event(),
                            )
                            .await
                        {
                            Ok(Some(event)) => ResolutionState::Resolved(event),
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
        let terminal_event = AgentEvent::TurnCancelled { usage };
        loop {
            match self
                .store
                .finish_turn_cancellation_and_append_event(turn.id, lease_token, Utc::now(), usage)
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
                            self.publish(turn.chat_id, event);
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
                    TurnFailureRetry::Permanent,
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
                            self.publish(turn.chat_id, event);
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
            let mut builder = DirBuilder::new();
            #[cfg(unix)]
            builder.mode(0o700);
            root_dir.create_dir_with(&chat_name, &builder)?;
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
        tools.register_foreground_sandbox_spawn();
        let call_id = openwave_core::CallId::new();
        let request = SandboxAgentSpawnRequest {
            call_id,
            child_run_id: openwave_core::AgentRunId::sandbox_for_spawn_call(call_id),
            task: "Research the error handling options.".into(),
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
