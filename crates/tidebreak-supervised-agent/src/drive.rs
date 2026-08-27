//! The turn state machine.
//!
//! One driver owns the whole conversation with the control endpoint: it
//! decides what the next turn is, runs it through the [`Engine`] seam, polls
//! on a fixed cadence while the turn runs, and reports every lifecycle
//! transition as an event. The ordering rules that matter live here:
//!
//! - The delivery cursor is acknowledged only *after* a message reached the
//!   engine — steered into a running turn, or carried by a turn that
//!   launched. A crash before the acknowledgement redelivers; acknowledging
//!   first would silently skip.
//! - A named refusal from the endpoint ends the process loudly. A transient
//!   fault retries on the poll cadence, but only up to a ceiling: an agent
//!   that cannot reach its supervisor for ten minutes is not supervised and
//!   must not pretend to be.

use std::collections::VecDeque;
use std::time::Duration;

use crate::control::{Control, Outbox, PollFailure};
use crate::engine::{Engine, SteerOutcome, TurnEnd, TurnHandle, TurnRequest, TurnSource};
use crate::inputs::{Inputs, RunMode, POLL_INTERVAL};
use crate::wire::{SupervisorMessage, SupervisorPoll};
use crate::{EXIT_CONTROL_FATAL, EXIT_ENGINE_FAILED};

/// Consecutive retryable poll failures before the agent gives up.
///
/// At the five-second cadence this is roughly ten minutes of unreachable
/// endpoint — long past any sidecar restart, and short enough that a wedged
/// network shows up as a failed pod instead of a silent one.
pub const MAX_CONSECUTIVE_POLL_FAILURES: u32 = 120;

/// Why the driver exited nonzero.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct DriveError {
    /// Process exit code.
    pub code: i32,
    /// What went wrong.
    pub message: String,
}

/// What the driver decided to do next.
enum NextAction {
    /// Run a turn.
    Run(TurnRequest),
    /// Nothing to run: poll idle and wait.
    Wait,
}

/// One iteration of the mid-turn select loop.
enum TurnStep {
    /// The turn ended.
    End(TurnEnd),
    /// The poll cadence ticked.
    Tick,
}

/// The supervised turn loop over one engine.
pub struct Driver<E> {
    control: Control,
    engine: E,
    outbox: Outbox,
    inbox: VecDeque<SupervisorMessage>,
    /// Highest sequence delivered to the engine; the poll cursor.
    delivered_through: Option<i64>,
    /// Highest sequence enqueued, so unacknowledged redeliveries dedupe.
    seen_through: i64,
    consecutive_failures: u32,
    stop_reason: Option<String>,
    acceptance_met: bool,
    mode: RunMode,
    task: String,
    turn: u32,
    max_turns: Option<u32>,
    ran_spawn_task: bool,
    last_turn_succeeded: bool,
    poll_interval: Duration,
}

impl<E: Engine> Driver<E> {
    /// Builds a driver from resolved inputs.
    pub fn new(control: Control, engine: E, inputs: &Inputs) -> Self {
        // A later incarnation already ran the spawn task; goal mode resumes
        // the goal, turn mode waits for steering.
        let resumed = inputs.starting_turn > 1;
        Self {
            control,
            engine,
            outbox: Outbox::default(),
            inbox: VecDeque::new(),
            delivered_through: None,
            seen_through: 0,
            consecutive_failures: 0,
            stop_reason: None,
            acceptance_met: false,
            mode: inputs.mode,
            task: inputs.task.clone(),
            turn: inputs.starting_turn,
            max_turns: inputs.max_turns,
            ran_spawn_task: resumed,
            last_turn_succeeded: resumed,
            poll_interval: POLL_INTERVAL,
        }
    }

    /// Overrides the poll cadence. Tests shrink it; production keeps the
    /// default.
    #[must_use]
    pub fn with_poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval;
        self
    }

    /// Runs the loop until the endpoint stops the agent or something fails.
    pub async fn run(mut self) -> Result<(), DriveError> {
        self.outbox.push(
            "supervisor_started",
            serde_json::json!({
                "harness": "custom",
                "agent": "tidebreak-supervised-agent",
            }),
        );
        if self.mode == RunMode::Goal && self.turn > 1 {
            // A resumed goal may already be complete or stopped. Read that
            // state before another engine turn can begin.
            self.poll(true).await?;
        }
        loop {
            if let Some(reason) = self.stop_reason.take() {
                return self.stopped(&reason).await;
            }
            match self.decide() {
                NextAction::Wait => {
                    self.poll(true).await?;
                    if self.stop_reason.is_some()
                        || (self.budget_allows(self.turn) && !self.inbox.is_empty())
                    {
                        continue;
                    }
                    tokio::time::sleep(self.poll_interval).await;
                }
                NextAction::Run(request) => self.run_turn(request).await?,
            }
        }
    }

    /// Whether the turn budget still allows `turn`.
    fn budget_allows(&self, turn: u32) -> bool {
        self.max_turns.is_none_or(|max| turn <= max)
    }

    /// Picks the next turn, or nothing.
    fn decide(&self) -> NextAction {
        if !self.budget_allows(self.turn) {
            // The budget is spent: park idle and keep polling, matching the
            // spawn contract. The endpoint decides what happens next.
            return NextAction::Wait;
        }
        if !self.ran_spawn_task {
            return NextAction::Run(TurnRequest {
                turn: self.turn,
                source: TurnSource::SpawnTask,
                input: self.task.clone(),
            });
        }
        if !self.inbox.is_empty() {
            let input = self
                .inbox
                .iter()
                .map(|message| message.body.as_str())
                .collect::<Vec<_>>()
                .join("\n\n");
            return NextAction::Run(TurnRequest {
                turn: self.turn,
                source: TurnSource::Inbox,
                input,
            });
        }
        if self.mode == RunMode::Goal && self.last_turn_succeeded && !self.acceptance_met {
            // The engine resumes its own session; there is no new input.
            return NextAction::Run(TurnRequest {
                turn: self.turn,
                source: TurnSource::GoalResume,
                input: String::new(),
            });
        }
        NextAction::Wait
    }

    /// Runs one turn to its end and reports how it went.
    async fn run_turn(&mut self, request: TurnRequest) -> Result<(), DriveError> {
        let turn = request.turn;
        let source = request.source;
        self.outbox
            .push("turn_started", serde_json::json!({ "turn": turn }));
        let mut handle = match self.engine.start_turn(request).await {
            Ok(handle) => handle,
            Err(error) => {
                // The inbox stays unacknowledged, so whatever this turn was
                // going to carry redelivers to the next incarnation.
                return self.engine_failed(&error.to_string()).await;
            }
        };
        if source == TurnSource::SpawnTask {
            self.ran_spawn_task = true;
        }
        if source == TurnSource::Inbox {
            // The turn launched carrying these bodies: now they count as
            // delivered.
            while let Some(message) = self.inbox.pop_front() {
                self.delivered_through = Some(message.seq);
            }
        }

        let end = loop {
            let step = tokio::select! {
                end = handle.wait() => TurnStep::End(end),
                () = tokio::time::sleep(self.poll_interval) => TurnStep::Tick,
            };
            match step {
                TurnStep::End(end) => break end,
                TurnStep::Tick => {
                    self.poll(false).await?;
                    if self.stop_reason.is_some() {
                        handle.interrupt().await;
                        continue;
                    }
                    if let Some(end) = self.steer_pending(handle.as_mut()).await {
                        break end;
                    }
                }
            }
        };

        match end {
            TurnEnd::Interrupted => {
                self.outbox
                    .push("turn_interrupted", serde_json::json!({ "turn": turn }));
                self.last_turn_succeeded = false;
            }
            TurnEnd::Completed { success } => {
                let may_resume = self.mode == RunMode::Goal
                    && success
                    && self.budget_allows(turn + 1)
                    && !self.acceptance_met;
                let mut payload = serde_json::json!({
                    "turn": turn,
                    "exit_code": if success { 0 } else { 1 },
                    "may_resume": may_resume,
                });
                if self.mode == RunMode::Turn {
                    payload["gate"] = serde_json::json!("turn_mode");
                }
                self.outbox.push("turn_completed", payload);
                self.last_turn_succeeded = success;
            }
            TurnEnd::Fatal { message } => {
                self.outbox.push(
                    "turn_completed",
                    serde_json::json!({
                        "turn": turn,
                        "exit_code": 1,
                        "may_resume": false,
                    }),
                );
                return self.engine_failed(&message).await;
            }
        }
        self.turn += 1;
        // Between turns the engine is momentarily idle; this poll flushes the
        // completion events and picks up anything the endpoint queued.
        self.poll(true).await
    }

    /// Delivers pending inbox messages into a running turn, in order.
    async fn steer_pending(&mut self, handle: &mut dyn TurnHandle) -> Option<TurnEnd> {
        while let Some(message) = self.inbox.front() {
            if message.interrupt {
                // The body is not delivered into this turn; it stays queued
                // and opens the next one.
                handle.interrupt().await;
                return None;
            }
            let body = message.body.clone();
            match handle.steer(body).await {
                SteerOutcome::Delivered => {
                    let message = self.inbox.pop_front().expect("front was just observed");
                    self.delivered_through = Some(message.seq);
                }
                // This engine takes input only between turns; the message
                // waits there.
                SteerOutcome::Refused => return None,
                // The poll completed after the turn did. Keep the message
                // queued so the next turn carries it.
                SteerOutcome::Ended(end) => return Some(end),
            }
        }
        None
    }

    /// Posts one poll, classifies the outcome, and absorbs instructions.
    async fn poll(&mut self, idle: bool) -> Result<(), DriveError> {
        let batch = self.outbox.take_batch();
        let mut poll = SupervisorPoll::new(idle, self.delivered_through);
        poll.events.clone_from(&batch);
        match self.control.poll(&poll).await {
            Ok(instructions) => {
                self.consecutive_failures = 0;
                for message in instructions.messages {
                    if message.seq > self.seen_through {
                        self.seen_through = message.seq;
                        self.inbox.push_back(message);
                    }
                }
                if instructions.stop && self.stop_reason.is_none() {
                    self.stop_reason = Some(
                        instructions
                            .stop_reason
                            .unwrap_or_else(|| "stop_requested".to_owned()),
                    );
                }
                self.acceptance_met = instructions.acceptance_met;
                Ok(())
            }
            Err(PollFailure::Retryable(message)) => {
                self.outbox.requeue(batch);
                self.consecutive_failures += 1;
                if self.consecutive_failures >= MAX_CONSECUTIVE_POLL_FAILURES {
                    return Err(DriveError {
                        code: EXIT_CONTROL_FATAL,
                        message: format!(
                            "the control endpoint failed {} consecutive polls; last: {message}",
                            self.consecutive_failures
                        ),
                    });
                }
                Ok(())
            }
            Err(fatal @ PollFailure::Fatal { .. }) => Err(DriveError {
                code: EXIT_CONTROL_FATAL,
                message: fatal.to_string(),
            }),
        }
    }

    /// Reports a clean stop and exits zero.
    async fn stopped(mut self, reason: &str) -> Result<(), DriveError> {
        self.outbox.push(
            "supervisor_stopped",
            serde_json::json!({ "reason": reason }),
        );
        self.flush().await;
        Ok(())
    }

    /// Reports an engine failure and exits with [`EXIT_ENGINE_FAILED`].
    async fn engine_failed(&mut self, message: &str) -> Result<(), DriveError> {
        self.outbox.push(
            "supervisor_stopped",
            serde_json::json!({ "reason": "engine_failed" }),
        );
        self.flush().await;
        Err(DriveError {
            code: EXIT_ENGINE_FAILED,
            message: format!("the engine failed: {message}"),
        })
    }

    /// Best-effort final delivery of whatever the outbox still holds.
    async fn flush(&mut self) {
        for _ in 0..3 {
            if self.outbox.is_empty() {
                return;
            }
            if self.poll(true).await.is_err() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use axum::extract::State;
    use axum::response::IntoResponse;
    use axum::Json;
    use tokio::sync::{mpsc, oneshot};

    use super::*;
    use crate::engine::EngineError;
    use crate::inputs::{resolve, RawInputs};

    /// One scripted delay for the next supervisor poll.
    struct PollBlock {
        started: oneshot::Sender<()>,
        release: oneshot::Receiver<()>,
    }

    /// Everything the mock supervisor records and serves.
    #[derive(Default)]
    struct MockSupervisor {
        events: Vec<(String, serde_json::Value)>,
        polls: Vec<serde_json::Value>,
        messages: Vec<(i64, String, bool)>,
        message_batches: usize,
        stop: Option<String>,
        reject: Option<(u16, String)>,
        block_next_poll: Option<PollBlock>,
    }

    type SharedSupervisor = Arc<Mutex<MockSupervisor>>;

    async fn poll_route(
        State(state): State<SharedSupervisor>,
        Json(body): Json<serde_json::Value>,
    ) -> axum::response::Response {
        let (reject, block) = {
            let mut supervisor = state.lock().unwrap();
            (supervisor.reject.clone(), supervisor.block_next_poll.take())
        };
        if let Some((status, code)) = reject {
            return (
                axum::http::StatusCode::from_u16(status).unwrap(),
                Json(serde_json::json!({
                    "error": code,
                    "error_description": "scripted refusal",
                })),
            )
                .into_response();
        }
        if let Some(PollBlock { started, release }) = block {
            let _ = started.send(());
            let _ = release.await;
        }
        let mut supervisor = state.lock().unwrap();
        supervisor.polls.push(body.clone());
        if let Some(events) = body.get("events").and_then(serde_json::Value::as_array) {
            for event in events {
                supervisor.events.push((
                    event["kind"].as_str().unwrap_or_default().to_owned(),
                    event["payload"].clone(),
                ));
            }
        }
        let delivered = body
            .get("delivered_through_seq")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0);
        let messages: Vec<serde_json::Value> = supervisor
            .messages
            .iter()
            .filter(|(seq, _, _)| *seq > delivered)
            .map(|(seq, message_body, interrupt)| {
                serde_json::json!({
                    "seq": seq,
                    "body": message_body,
                    "interrupt": interrupt,
                    "created_at": "2026-08-27T00:00:00Z",
                })
            })
            .collect();
        if !messages.is_empty() {
            supervisor.message_batches += 1;
        }
        Json(serde_json::json!({
            "sandbox_id": "018f0000-0000-7000-8000-000000000000",
            "state": if supervisor.stop.is_some() { "completing" } else { "running" },
            "stop": supervisor.stop.is_some(),
            "stop_reason": supervisor.stop,
            "cursor": delivered,
            "messages": messages,
        }))
        .into_response()
    }

    async fn start_supervisor() -> (SharedSupervisor, String) {
        let state: SharedSupervisor = Arc::default();
        let app = axum::Router::new()
            .route("/supervisor/poll", axum::routing::post(poll_route))
            .with_state(Arc::clone(&state));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (state, format!("http://{address}"))
    }

    /// A scripted engine: the test feeds turn ends through a channel.
    struct EngineState {
        turns: Mutex<Vec<TurnRequest>>,
        steers: Mutex<Vec<String>>,
        refuse_steer: AtomicBool,
        ends: tokio::sync::Mutex<mpsc::UnboundedReceiver<TurnEnd>>,
        end_sender: mpsc::UnboundedSender<TurnEnd>,
    }

    #[derive(Clone)]
    struct MockEngine {
        state: Arc<EngineState>,
    }

    impl MockEngine {
        fn new() -> Self {
            let (end_sender, ends) = mpsc::unbounded_channel();
            Self {
                state: Arc::new(EngineState {
                    turns: Mutex::new(Vec::new()),
                    steers: Mutex::new(Vec::new()),
                    refuse_steer: AtomicBool::new(false),
                    ends: tokio::sync::Mutex::new(ends),
                    end_sender,
                }),
            }
        }

        fn finish(&self, end: TurnEnd) {
            self.state.end_sender.send(end).unwrap();
        }

        fn turns(&self) -> Vec<TurnRequest> {
            self.state.turns.lock().unwrap().clone()
        }
    }

    struct MockTurnHandle {
        state: Arc<EngineState>,
    }

    #[async_trait]
    impl TurnHandle for MockTurnHandle {
        async fn wait(&mut self) -> TurnEnd {
            let mut ends = self.state.ends.lock().await;
            ends.recv().await.unwrap_or(TurnEnd::Fatal {
                message: "the scripted engine hung up".to_owned(),
            })
        }

        async fn steer(&mut self, body: String) -> SteerOutcome {
            let mut ends = self.state.ends.lock().await;
            match ends.try_recv() {
                Ok(end) => return SteerOutcome::Ended(end),
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    return SteerOutcome::Ended(TurnEnd::Fatal {
                        message: "the scripted engine hung up".to_owned(),
                    });
                }
                Err(mpsc::error::TryRecvError::Empty) => {}
            }
            drop(ends);
            if self.state.refuse_steer.load(Ordering::SeqCst) {
                return SteerOutcome::Refused;
            }
            self.state.steers.lock().unwrap().push(body);
            SteerOutcome::Delivered
        }

        async fn interrupt(&mut self) {
            self.state.end_sender.send(TurnEnd::Interrupted).unwrap();
        }
    }

    #[async_trait]
    impl Engine for MockEngine {
        async fn start_turn(
            &mut self,
            request: TurnRequest,
        ) -> Result<Box<dyn TurnHandle>, EngineError> {
            self.state.turns.lock().unwrap().push(request);
            Ok(Box::new(MockTurnHandle {
                state: Arc::clone(&self.state),
            }))
        }
    }

    fn inputs(mode: &str, max_turns: Option<&str>) -> Inputs {
        resolve(RawInputs {
            task: Some("do the thing".to_owned()),
            workspace: Some("/workspace".to_owned()),
            mode: Some(mode.to_owned()),
            max_turns: max_turns.map(str::to_owned),
            ..RawInputs::default()
        })
        .unwrap()
    }

    fn driver(engine: MockEngine, url: &str, inputs: &Inputs) -> Driver<MockEngine> {
        Driver::new(Control::new(url), engine, inputs).with_poll_interval(Duration::from_millis(10))
    }

    /// Polls a condition on the supervisor until it holds.
    async fn wait_for(state: &SharedSupervisor, check: impl Fn(&MockSupervisor) -> bool) {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if check(&state.lock().unwrap()) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the supervisor never observed the expected state");
    }

    fn event_kinds(state: &SharedSupervisor) -> Vec<String> {
        state
            .lock()
            .unwrap()
            .events
            .iter()
            .map(|(kind, _)| kind.clone())
            .collect()
    }

    fn event_payload(state: &SharedSupervisor, kind: &str, index: usize) -> serde_json::Value {
        state
            .lock()
            .unwrap()
            .events
            .iter()
            .filter(|(event_kind, _)| event_kind == kind)
            .nth(index)
            .map(|(_, payload)| payload.clone())
            .unwrap_or_else(|| panic!("no {kind} event at index {index}"))
    }

    #[tokio::test]
    async fn turn_mode_runs_the_task_then_waits_for_steering() {
        let (state, url) = start_supervisor().await;
        let engine = MockEngine::new();
        engine.finish(TurnEnd::Completed { success: true });
        let run = tokio::spawn(driver(engine.clone(), &url, &inputs("turn", None)).run());

        wait_for(&state, |supervisor| {
            supervisor
                .events
                .iter()
                .any(|(kind, _)| kind == "turn_completed")
        })
        .await;
        assert_eq!(engine.turns()[0].source, TurnSource::SpawnTask);
        assert_eq!(engine.turns()[0].input, "do the thing");
        let completed = event_payload(&state, "turn_completed", 0);
        assert_eq!(completed["gate"], "turn_mode");
        assert_eq!(completed["may_resume"], false);

        state
            .lock()
            .unwrap()
            .messages
            .push((1, "now do the next thing".to_owned(), false));
        wait_for(&state, |supervisor| {
            supervisor.polls.iter().any(|poll| {
                poll.get("delivered_through_seq")
                    .and_then(serde_json::Value::as_i64)
                    == Some(1)
            })
        })
        .await;
        assert_eq!(engine.turns().len(), 2);
        assert_eq!(engine.turns()[1].source, TurnSource::Inbox);
        assert_eq!(engine.turns()[1].input, "now do the next thing");

        engine.finish(TurnEnd::Completed { success: true });
        wait_for(&state, |supervisor| {
            supervisor
                .events
                .iter()
                .filter(|(kind, _)| kind == "turn_completed")
                .count()
                == 2
        })
        .await;
        state.lock().unwrap().stop = Some("cancelled".to_owned());
        run.await.unwrap().unwrap();
        let kinds = event_kinds(&state);
        assert_eq!(kinds[0], "supervisor_started");
        assert_eq!(kinds.last().map(String::as_str), Some("supervisor_stopped"));
        assert_eq!(
            event_payload(&state, "supervisor_stopped", 0)["reason"],
            "cancelled"
        );
    }

    #[tokio::test]
    async fn goal_mode_resumes_takes_steering_and_honors_stop() {
        let (state, url) = start_supervisor().await;
        let engine = MockEngine::new();
        engine.finish(TurnEnd::Completed { success: true });
        let run = tokio::spawn(driver(engine.clone(), &url, &inputs("goal", None)).run());

        wait_for(&state, |supervisor| {
            supervisor
                .events
                .iter()
                .any(|(kind, _)| kind == "turn_completed")
        })
        .await;
        let completed = event_payload(&state, "turn_completed", 0);
        assert_eq!(completed["may_resume"], true);
        assert!(
            completed.get("gate").is_none(),
            "a goal continuation names no gate"
        );

        // The second turn is a goal resume; a mid-turn message steers into it.
        wait_for(&state, |_| engine.turns().len() == 2).await;
        assert_eq!(engine.turns()[1].source, TurnSource::GoalResume);
        state
            .lock()
            .unwrap()
            .messages
            .push((1, "focus on the tests".to_owned(), false));
        wait_for(&state, |supervisor| {
            supervisor.polls.iter().any(|poll| {
                poll.get("delivered_through_seq")
                    .and_then(serde_json::Value::as_i64)
                    == Some(1)
            })
        })
        .await;
        assert_eq!(
            engine.state.steers.lock().unwrap().as_slice(),
            ["focus on the tests"]
        );

        state.lock().unwrap().stop = Some("cancelled".to_owned());
        run.await.unwrap().unwrap();
        let kinds = event_kinds(&state);
        assert!(kinds.iter().any(|kind| kind == "turn_interrupted"));
        assert_eq!(kinds.last().map(String::as_str), Some("supervisor_stopped"));
    }

    #[tokio::test]
    async fn a_turn_that_ends_during_poll_keeps_the_message_for_the_next_turn() {
        let (state, url) = start_supervisor().await;
        let (started_tx, started_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        {
            let mut supervisor = state.lock().unwrap();
            supervisor.block_next_poll = Some(PollBlock {
                started: started_tx,
                release: release_rx,
            });
            supervisor
                .messages
                .push((1, "after the turn".to_owned(), false));
        }
        let engine = MockEngine::new();
        let run = tokio::spawn(driver(engine.clone(), &url, &inputs("turn", None)).run());

        tokio::time::timeout(Duration::from_secs(10), started_rx)
            .await
            .expect("the driver never started its poll")
            .expect("the poll dropped its start signal");
        engine.finish(TurnEnd::Completed { success: true });
        release_tx.send(()).expect("release the blocked poll");

        wait_for(&state, |_| engine.turns().len() == 2).await;
        assert!(engine.state.steers.lock().unwrap().is_empty());
        assert_eq!(engine.turns()[1].source, TurnSource::Inbox);
        assert_eq!(engine.turns()[1].input, "after the turn");

        engine.finish(TurnEnd::Completed { success: true });
        wait_for(&state, |supervisor| {
            supervisor
                .events
                .iter()
                .filter(|(kind, _)| kind == "turn_completed")
                .count()
                == 2
        })
        .await;
        state.lock().unwrap().stop = Some("cancelled".to_owned());
        run.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn an_interrupt_message_preempts_the_turn_and_opens_the_next() {
        let (state, url) = start_supervisor().await;
        let engine = MockEngine::new();
        let run = tokio::spawn(driver(engine.clone(), &url, &inputs("turn", None)).run());

        // The spawn turn is running with no scripted end; an interrupt
        // message must preempt it.
        wait_for(&state, |_| engine.turns().len() == 1).await;
        state
            .lock()
            .unwrap()
            .messages
            .push((1, "change course".to_owned(), true));
        wait_for(&state, |supervisor| {
            supervisor
                .events
                .iter()
                .any(|(kind, _)| kind == "turn_interrupted")
        })
        .await;

        // The interrupt body was not delivered into the dead turn; it opens
        // the next one, and only then is the cursor acknowledged.
        wait_for(&state, |_| engine.turns().len() == 2).await;
        assert_eq!(engine.turns()[1].source, TurnSource::Inbox);
        assert_eq!(engine.turns()[1].input, "change course");
        wait_for(&state, |supervisor| {
            supervisor.polls.iter().any(|poll| {
                poll.get("delivered_through_seq")
                    .and_then(serde_json::Value::as_i64)
                    == Some(1)
            })
        })
        .await;

        engine.finish(TurnEnd::Completed { success: true });
        state.lock().unwrap().stop = Some("cancelled".to_owned());
        run.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn an_exhausted_turn_budget_parks_the_agent_idle() {
        let (state, url) = start_supervisor().await;
        let engine = MockEngine::new();
        engine.finish(TurnEnd::Completed { success: true });
        let run = tokio::spawn(
            driver(engine.clone(), &url, &inputs("goal", Some("1")))
                .with_poll_interval(Duration::from_millis(100))
                .run(),
        );

        wait_for(&state, |supervisor| {
            supervisor
                .events
                .iter()
                .any(|(kind, _)| kind == "turn_completed")
        })
        .await;
        assert_eq!(
            event_payload(&state, "turn_completed", 0)["may_resume"],
            false
        );

        state
            .lock()
            .unwrap()
            .messages
            .push((1, "wait for another budget".to_owned(), false));
        wait_for(&state, |supervisor| supervisor.message_batches == 1).await;
        let polls_after_message = state.lock().unwrap().polls.len();

        // The queued message cannot consume another turn. It must not remove
        // the idle cadence while the agent waits for a stop.
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(state.lock().unwrap().polls.len(), polls_after_message);
        assert_eq!(engine.turns().len(), 1);

        state.lock().unwrap().stop = Some("expired".to_owned());
        run.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn a_resumed_goal_polls_before_starting_another_turn() {
        let (state, url) = start_supervisor().await;
        state.lock().unwrap().stop = Some("acceptance_met".to_owned());
        let engine = MockEngine::new();
        let mut resumed = inputs("goal", None);
        resumed.starting_turn = 2;

        driver(engine.clone(), &url, &resumed).run().await.unwrap();

        assert!(engine.turns().is_empty());
        assert_eq!(
            event_payload(&state, "supervisor_stopped", 0)["reason"],
            "acceptance_met"
        );
    }

    #[tokio::test]
    async fn a_named_refusal_ends_the_run_loudly() {
        let (state, url) = start_supervisor().await;
        state.lock().unwrap().reject = Some((400, "sandbox_event_invalid".to_owned()));
        let engine = MockEngine::new();
        engine.finish(TurnEnd::Completed { success: true });
        let error = driver(engine, &url, &inputs("goal", None))
            .run()
            .await
            .unwrap_err();
        assert_eq!(error.code, EXIT_CONTROL_FATAL);
        assert!(error.message.contains("sandbox_event_invalid"));
    }

    #[tokio::test]
    async fn an_engine_failure_reports_before_exiting_nonzero() {
        let (state, url) = start_supervisor().await;
        let engine = MockEngine::new();
        engine.finish(TurnEnd::Fatal {
            message: "the engine binary is gone".to_owned(),
        });
        let error = driver(engine, &url, &inputs("goal", None))
            .run()
            .await
            .unwrap_err();
        assert_eq!(error.code, EXIT_ENGINE_FAILED);
        assert!(error.message.contains("the engine binary is gone"));
        let kinds = event_kinds(&state);
        assert!(kinds.iter().any(|kind| kind == "turn_completed"));
        assert_eq!(kinds.last().map(String::as_str), Some("supervisor_stopped"));
        assert_eq!(
            event_payload(&state, "supervisor_stopped", 0)["reason"],
            "engine_failed"
        );
    }

    #[tokio::test]
    async fn a_refused_steer_keeps_the_message_for_the_next_turn() {
        let (state, url) = start_supervisor().await;
        let engine = MockEngine::new();
        engine.state.refuse_steer.store(true, Ordering::SeqCst);
        let run = tokio::spawn(driver(engine.clone(), &url, &inputs("turn", None)).run());

        wait_for(&state, |_| engine.turns().len() == 1).await;
        state
            .lock()
            .unwrap()
            .messages
            .push((1, "for the next turn".to_owned(), false));
        // Let several cadences pass: the refused steer must not acknowledge.
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(engine.state.steers.lock().unwrap().is_empty());
        assert!(!state.lock().unwrap().polls.iter().any(|poll| {
            poll.get("delivered_through_seq")
                .and_then(serde_json::Value::as_i64)
                == Some(1)
        }));

        engine.finish(TurnEnd::Completed { success: true });
        wait_for(&state, |_| engine.turns().len() == 2).await;
        assert_eq!(engine.turns()[1].input, "for the next turn");
        engine.finish(TurnEnd::Completed { success: true });
        state.lock().unwrap().stop = Some("cancelled".to_owned());
        run.await.unwrap().unwrap();
    }
}
