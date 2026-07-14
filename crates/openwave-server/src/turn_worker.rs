//! Supervised execution for durably claimed chat turns.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use futures::channel::mpsc::unbounded;
use futures::StreamExt;
use openwave_core::{
    Agent, AgentConfig, AgentError, AgentEvent, AgentTurnOutcome, MessageId,
    RecordTurnFailureOutcome, Result, SequencedEvent, Store, ToolRegistry, TurnFailureRetry,
    TurnId, TurnRun, TurnRunStatus,
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
    agent_config: AgentConfig,
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum LeaseState {
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
        agent_config: AgentConfig,
        config: TurnWorkerConfig,
    ) -> Self {
        assert!(!config.lease.is_zero());
        assert!(!config.heartbeat.is_zero());
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
            agent_config,
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
        // Intermediate tool/message effects are not yet lease-CAS fenced. Keep
        // execution explicitly single-attempt so a crash can fail conservatively
        // but can never replay a filesystem or external tool side effect.
        if turn.max_attempts != 1 {
            return self
                .record_failure(
                    &turn,
                    lease_token,
                    "unsupported_retry_policy",
                    "turn execution requires max_attempts = 1",
                )
                .await;
        }
        let Some(chat) = self.store.get_chat(turn.chat_id).await? else {
            return self
                .record_failure(
                    &turn,
                    lease_token,
                    "chat_missing",
                    "claimed turn chat is missing",
                )
                .await;
        };
        let Some(active) = self.signals.register(turn.chat_id, turn.id, lease_token) else {
            return Err(AgentError::msg(format!(
                "turn {} already has a conflicting local worker",
                turn.id
            )));
        };
        let cancel = active.cancel_token();
        match self.renew_lease(&turn, lease_token).await {
            LeaseState::Running => {}
            LeaseState::Cancelling => {
                drop(active);
                return self
                    .acknowledge_cancellation(&turn, lease_token, openwave_core::Usage::default())
                    .await;
            }
            LeaseState::Lost => return Ok(TurnWorkerOutcome::LeaseLost(turn.id)),
        }
        let mut heartbeat = AbortOnDrop(tokio::spawn(self.clone().heartbeat_lease(
            turn.clone(),
            lease_token,
            cancel.clone(),
        )));
        let mut heartbeat_open = true;

        let started = AgentEvent::TurnStarted { turn_id: turn.id };
        match self.append_event(&turn, lease_token, 1, &started).await? {
            EventAppend::Committed => {}
            EventAppend::Cancelling => {
                drop(active);
                return self
                    .acknowledge_cancellation(&turn, lease_token, openwave_core::Usage::default())
                    .await;
            }
            EventAppend::LeaseLost => return Ok(TurnWorkerOutcome::LeaseLost(turn.id)),
        }

        let mut config = self.agent_config.clone();
        config.model = turn.model.clone();
        let provider = self.resolver.resolve().await;
        let agent = Agent::new(provider, self.tools.clone(), self.store.clone(), config)
            .with_approvals(self.approvals.clone())
            .with_cancel(cancel.clone())
            .with_steer(active.steer_inbox());
        let output_message_id = MessageId::new();
        let (events_tx, mut events_rx) = unbounded();
        let mut drive = AbortOnDrop(tokio::spawn(async move {
            agent
                .run_claimed_turn(&chat, turn.id, output_message_id, &events_tx)
                .await
        }));
        let mut drive_result = None;
        let mut channel_open = true;
        let mut ordinal = 2_i32;

        while drive_result.is_none() || channel_open {
            tokio::select! {
                result = &mut drive.0, if drive_result.is_none() => {
                    drive_result = Some(result);
                }
                event = events_rx.next(), if channel_open => {
                    match event {
                        Some(event) => {
                            match self.append_event(&turn, lease_token, ordinal, &event).await? {
                                EventAppend::Committed => {
                                    ordinal = ordinal.checked_add(1).ok_or_else(|| {
                                        AgentError::msg(format!("turn {} event ordinal exhausted", turn.id))
                                    })?;
                                }
                                EventAppend::Cancelling => cancel.cancel(),
                                EventAppend::LeaseLost => {
                                    drive.abort_and_wait().await;
                                    if heartbeat_open {
                                        heartbeat.abort_and_wait().await;
                                    }
                                    return Ok(TurnWorkerOutcome::LeaseLost(turn.id));
                                }
                            }
                        }
                        None => channel_open = false,
                    }
                }
                result = &mut heartbeat.0, if heartbeat_open => {
                    match result {
                        Ok(HeartbeatOutcome::Cancelling) => heartbeat_open = false,
                        Ok(HeartbeatOutcome::LeaseLost) | Err(_) => {
                            drive.abort_and_wait().await;
                            return Ok(TurnWorkerOutcome::LeaseLost(turn.id));
                        }
                    }
                }
            }
        }
        // Freeze periodic lease writes before terminal CAS. A concurrent
        // heartbeat would legitimately change `updated_at` between the
        // resolution read and update and make this worker race itself.
        if heartbeat_open {
            heartbeat.abort_and_wait().await;
        }
        let drive_result = match drive_result.expect("drive completed before its channel closed") {
            Ok(result) => result,
            Err(error) => Err(AgentError::msg(format!("agent task stopped: {error}"))),
        };
        // Prove a fresh full lease after stopping the periodic task. This both
        // closes near-expiry ambiguous-claim recovery and gives exact terminal
        // retries their full resolution window without a self-racing heartbeat.
        match self.renew_lease(&turn, lease_token).await {
            LeaseState::Running => {}
            LeaseState::Cancelling => {
                let usage = match &drive_result {
                    Ok(AgentTurnOutcome::Completed { usage, .. })
                    | Ok(AgentTurnOutcome::Cancelled { usage }) => *usage,
                    Err(_) => openwave_core::Usage::default(),
                };
                drop(active);
                return self
                    .acknowledge_cancellation(&turn, lease_token, usage)
                    .await;
            }
            LeaseState::Lost => return Ok(TurnWorkerOutcome::LeaseLost(turn.id)),
        }
        drop(active);

        match drive_result {
            Ok(AgentTurnOutcome::Completed {
                output,
                usage,
                stop_reason,
            }) => {
                if output.content.contains('\0') {
                    return self
                        .record_failure(
                            &turn,
                            lease_token,
                            "invalid_agent_output",
                            "agent output contained a NUL character",
                        )
                        .await;
                }
                let terminal_event = AgentEvent::TurnCompleted { usage, stop_reason };
                loop {
                    match self
                        .store
                        .complete_turn_run_and_append_event(
                            turn.id,
                            lease_token,
                            Utc::now(),
                            &output,
                            usage,
                            stop_reason,
                        )
                        .await
                    {
                        Ok(Some(resolution)) => {
                            if let Some(event) = resolution.terminal_event {
                                self.publish(turn.chat_id, event);
                            }
                            return Ok(TurnWorkerOutcome::Completed(turn.id));
                        }
                        Ok(None) => break,
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
                                ResolutionState::Cancelling => break,
                                ResolutionState::Lost => {
                                    return Ok(TurnWorkerOutcome::LeaseLost(turn.id));
                                }
                            }
                        }
                    }
                }
                self.acknowledge_cancellation(&turn, lease_token, usage)
                    .await
            }
            Ok(AgentTurnOutcome::Cancelled { usage }) => {
                self.acknowledge_cancellation(&turn, lease_token, usage)
                    .await
            }
            Err(error) => {
                if self.is_cancelling_retry(&turn, lease_token).await {
                    return self
                        .acknowledge_cancellation(
                            &turn,
                            lease_token,
                            openwave_core::Usage::default(),
                        )
                        .await;
                }
                self.record_failure(&turn, lease_token, "agent_error", &error.to_string())
                    .await
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
                Ok(None) => return Ok(TurnWorkerOutcome::LeaseLost(turn.id)),
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
                                .acknowledge_cancellation(
                                    turn,
                                    lease_token,
                                    openwave_core::Usage::default(),
                                )
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
