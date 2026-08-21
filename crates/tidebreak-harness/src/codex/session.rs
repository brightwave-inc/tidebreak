//! Long-lived `codex app-server --stdio` child.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{ChildStdin, ChildStdout, Command};
use tokio::sync::{oneshot, Mutex as AsyncMutex, Notify};
use tokio::time::{timeout, Instant};
use tracing::warn;

use crate::browser_channel::apply_child_env_tokio;
use crate::codex::parse::CodexStreamParser;
use crate::launch::{validate_launch_plan, LaunchPlan};
use crate::{
    spawn_process_tree, ApprovalDecision, BrowserChannelSpec, HarnessApprovalRef, HarnessError,
    HarnessEvent, HarnessSession, ProcessTreeChild, SessionSpec, StreamBudget, StreamLineBuffer,
    TurnInput, TurnOutcome,
};
use tidebreak_core::{CodePermissionMode, MAX_NOTICE_CHARS};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);
const CONTROL_RPC_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_STDERR_BYTES: usize = 64 * 1_024;

/// Live Codex session: one app-server child for the session lifetime.
pub struct CodexSession {
    spec: SessionSpec,
    resume_ref: Mutex<Option<String>>,
    /// Whether a turn has actually run on this thread. Codex only writes the
    /// thread's rollout once a turn starts, so a thread id from
    /// `thread/start` alone is not resumable and must not be handed out as a
    /// resume ref — see [`HarnessSession::resume_ref`] below.
    thread_ran_a_turn: AtomicBool,
    /// Detail from an engine error saying the resumed thread is gone.
    resume_lost: Mutex<Option<String>>,
    child: AsyncMutex<Option<ProcessTreeChild>>,
    child_pid: AtomicU32,
    stdin: Option<Arc<AsyncMutex<ChildStdin>>>,
    stdout: Option<Arc<AsyncMutex<StdoutReader>>>,
    parser: Arc<Mutex<CodexStreamParser>>,
    next_id: AtomicI64,
    control_state: Arc<Mutex<ControlState>>,
    control_state_changed: Arc<Notify>,
    pending_approvals: Mutex<HashMap<String, Value>>,
}

struct StdoutReader {
    stdout: ChildStdout,
    lines: StreamLineBuffer,
}

struct ControlState {
    turn: ControlTurn,
    pending: HashMap<i64, PendingSteer>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ControlTurn {
    Idle,
    Starting,
    Active(String),
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingWriteState {
    Queued,
    Writing,
    Written,
}

struct PendingSteer {
    expected_turn_id: String,
    text: String,
    reply: Option<oneshot::Sender<Result<(), HarnessError>>>,
    deadline: Option<Instant>,
    write_state: PendingWriteState,
    accept_response: bool,
}

struct ControlRegistration<'a> {
    session: &'a CodexSession,
    rpc_id: i64,
    armed: bool,
}

impl ControlRegistration<'_> {
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ControlRegistration<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let removed = {
            let mut state = self
                .session
                .control_state
                .lock()
                .expect("codex control state");
            if state
                .pending
                .get(&self.rpc_id)
                .is_some_and(|pending| pending.write_state == PendingWriteState::Queued)
            {
                state.pending.remove(&self.rpc_id)
            } else {
                None
            }
        };
        if removed.is_some() {
            self.session
                .parser
                .lock()
                .expect("codex parser")
                .forget_outbound(&json!(self.rpc_id));
            self.session.control_state_changed.notify_one();
        }
    }
}

impl CodexSession {
    pub(super) fn new(spec: SessionSpec) -> Self {
        let resume_ref = spec.resume_ref.clone();
        Self {
            spec,
            resume_ref: Mutex::new(resume_ref),
            thread_ran_a_turn: AtomicBool::new(false),
            resume_lost: Mutex::new(None),
            child: AsyncMutex::new(None),
            child_pid: AtomicU32::new(0),
            stdin: None,
            stdout: None,
            parser: Arc::new(Mutex::new(CodexStreamParser::new())),
            next_id: AtomicI64::new(1),
            control_state: Arc::new(Mutex::new(ControlState {
                turn: ControlTurn::Idle,
                pending: HashMap::new(),
            })),
            control_state_changed: Arc::new(Notify::new()),
            pending_approvals: Mutex::new(HashMap::new()),
        }
    }

    /// The detail of a lost resume observed on the stream, when any.
    fn lost_resume(&self) -> Option<String> {
        self.resume_lost.lock().expect("codex resume lost").clone()
    }

    fn next_rpc_id(&self) -> i64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    async fn write_message(&self, message: &Value) -> Result<(), HarnessError> {
        let Some(stdin) = &self.stdin else {
            return Err(HarnessError::Other("engine child has no stdin".into()));
        };
        let mut line = serde_json::to_vec(message)
            .map_err(|err| HarnessError::Other(format!("serialize rpc: {err}")))?;
        line.push(b'\n');
        let mut guard = stdin.lock().await;
        guard.write_all(&line).await?;
        guard.flush().await?;
        Ok(())
    }

    async fn request(&self, method: &str, params: Value) -> Result<i64, HarnessError> {
        let id = self.next_rpc_id();
        self.parser
            .lock()
            .expect("codex parser")
            .note_outbound(&json!(id), method);
        self.write_message(&json!({ "id": id, "method": method, "params": params }))
            .await?;
        Ok(id)
    }

    /// Admit one native steer while `run_turn` remains the sole stdout reader.
    /// The turn id and admission gate are read under the same lock, so a
    /// terminal event cannot leave a request registered for the following
    /// turn. The stdin writer is detached: cancellation before it owns the
    /// write removes a queued request, while cancellation after that point
    /// leaves the stream reader responsible for the acknowledgement and
    /// `UserSteered` event.
    async fn request_steer(&self, thread_id: String, text: String) -> Result<(), HarnessError> {
        let Some(stdin) = self.stdin.clone() else {
            return Err(HarnessError::SteeringRejected(
                "the engine child has no stdin".into(),
            ));
        };
        let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
        let (rpc_id, message, rx) = loop {
            let changed = self.control_state_changed.notified();
            let admitted = {
                let mut state = self.control_state.lock().expect("codex control state");
                match &state.turn {
                    ControlTurn::Active(turn_id) => {
                        let expected_turn_id = turn_id.clone();
                        let rpc_id = self.next_rpc_id();
                        let client_message_id = format!("tidebreak-steer-{rpc_id}");
                        let message = steer_request(
                            rpc_id,
                            &thread_id,
                            &expected_turn_id,
                            &text,
                            &client_message_id,
                        );
                        let (tx, rx) = oneshot::channel();
                        self.parser
                            .lock()
                            .expect("codex parser")
                            .note_outbound(&json!(rpc_id), "turn/steer");
                        state.pending.insert(
                            rpc_id,
                            PendingSteer {
                                expected_turn_id,
                                text: text.clone(),
                                reply: Some(tx),
                                deadline: None,
                                write_state: PendingWriteState::Queued,
                                accept_response: true,
                            },
                        );
                        Some(Ok((rpc_id, message, rx)))
                    }
                    ControlTurn::Starting => None,
                    ControlTurn::Idle | ControlTurn::Closed => {
                        Some(Err(HarnessError::SteeringRejected(
                            "the active turn finished before steering was admitted".into(),
                        )))
                    }
                }
            };
            if let Some(admitted) = admitted {
                break admitted?;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() || timeout(remaining, changed).await.is_err() {
                return Err(HarnessError::SteeringRejected(
                    "the active turn was not acknowledged by the engine".into(),
                ));
            }
        };
        self.control_state_changed.notify_one();

        let mut registration = ControlRegistration {
            session: self,
            rpc_id,
            armed: true,
        };
        spawn_control_write(
            stdin,
            Arc::clone(&self.control_state),
            Arc::clone(&self.control_state_changed),
            Arc::clone(&self.parser),
            rpc_id,
            message,
        );
        let result = rx.await.unwrap_or_else(|_| {
            Err(HarnessError::SteeringRejected(
                "the engine dropped the steering response".into(),
            ))
        });
        registration.disarm();
        result
    }

    fn begin_control_turn(&self) {
        let pending = {
            let mut state = self.control_state.lock().expect("codex control state");
            state.turn = ControlTurn::Starting;
            std::mem::take(&mut state.pending)
        };
        for (id, mut pending) in pending {
            if let Some(reply) = pending.reply.take() {
                let _ = reply.send(Err(HarnessError::SteeringRejected(
                    "the prior turn ended before steering was accepted".into(),
                )));
            }
            self.parser
                .lock()
                .expect("codex parser")
                .forget_outbound(&json!(id));
        }
        self.control_state_changed.notify_waiters();
    }

    fn activate_control_turn(&self, turn_id: &str) {
        let changed = {
            let mut state = self.control_state.lock().expect("codex control state");
            match &state.turn {
                ControlTurn::Starting => {
                    state.turn = ControlTurn::Active(turn_id.to_owned());
                    true
                }
                ControlTurn::Active(active) if active == turn_id => false,
                ControlTurn::Idle | ControlTurn::Active(_) | ControlTurn::Closed => false,
            }
        };
        if changed {
            self.control_state_changed.notify_waiters();
        }
    }

    /// Close admission before a terminal event is emitted. Requests that have
    /// not started writing are removed immediately. An in-flight write becomes
    /// a bounded tombstone: its caller is rejected now, and the sole stream
    /// reader still consumes its eventual response without emitting a steer.
    fn close_control_turn_for_terminal(&self, detail: &str) {
        let removed = {
            let mut state = self.control_state.lock().expect("codex control state");
            state.turn = ControlTurn::Closed;
            let queued_ids = state
                .pending
                .iter()
                .filter_map(|(id, pending)| {
                    (pending.write_state == PendingWriteState::Queued).then_some(*id)
                })
                .collect::<Vec<_>>();
            let removed = queued_ids
                .into_iter()
                .filter_map(|id| state.pending.remove(&id).map(|pending| (id, pending)))
                .collect::<Vec<_>>();
            for pending in state.pending.values_mut() {
                pending.accept_response = false;
                if let Some(reply) = pending.reply.take() {
                    let _ = reply.send(Err(HarnessError::SteeringRejected(detail.into())));
                }
            }
            removed
        };
        for (id, mut pending) in removed {
            if let Some(reply) = pending.reply.take() {
                let _ = reply.send(Err(HarnessError::SteeringRejected(detail.into())));
            }
            self.parser
                .lock()
                .expect("codex parser")
                .forget_outbound(&json!(id));
        }
        self.control_state_changed.notify_waiters();
    }

    fn abort_control_turn(&self, detail: &str) {
        let pending = {
            let mut state = self.control_state.lock().expect("codex control state");
            state.turn = ControlTurn::Closed;
            std::mem::take(&mut state.pending)
        };
        for (id, mut pending) in pending {
            if let Some(reply) = pending.reply.take() {
                let _ = reply.send(Err(HarnessError::SteeringRejected(detail.into())));
            }
            self.parser
                .lock()
                .expect("codex parser")
                .forget_outbound(&json!(id));
        }
        self.control_state_changed.notify_waiters();
    }

    fn next_control_deadline(&self) -> Option<Instant> {
        self.control_state
            .lock()
            .expect("codex control state")
            .pending
            .values()
            .filter_map(|pending| pending.deadline)
            .min()
    }

    fn expire_control_requests(&self) {
        let now = Instant::now();
        let expired = {
            let mut state = self.control_state.lock().expect("codex control state");
            let ids = state
                .pending
                .iter()
                .filter_map(|(id, pending)| {
                    pending
                        .deadline
                        .is_some_and(|deadline| deadline <= now)
                        .then_some(*id)
                })
                .collect::<Vec<_>>();
            ids.into_iter()
                .filter_map(|id| state.pending.remove(&id).map(|pending| (id, pending)))
                .collect::<Vec<_>>()
        };
        for (id, mut pending) in expired {
            if let Some(reply) = pending.reply.take() {
                let _ = reply.send(Err(HarnessError::SteeringRejected(
                    "timed out waiting for the engine to accept steering".into(),
                )));
            }
            self.parser
                .lock()
                .expect("codex parser")
                .forget_outbound(&json!(id));
        }
        self.control_state_changed.notify_waiters();
    }

    fn controls_pending(&self) -> bool {
        !self
            .control_state
            .lock()
            .expect("codex control state")
            .pending
            .is_empty()
    }

    fn active_control_turn_id(&self) -> Option<String> {
        let state = self.control_state.lock().expect("codex control state");
        match &state.turn {
            ControlTurn::Active(turn_id) => Some(turn_id.clone()),
            ControlTurn::Idle | ControlTurn::Starting | ControlTurn::Closed => None,
        }
    }

    async fn notify(&self, method: &str, params: Option<Value>) -> Result<(), HarnessError> {
        let message = match params {
            Some(params) => json!({ "method": method, "params": params }),
            None => json!({ "method": method }),
        };
        self.write_message(&message).await
    }
}

/// Argv for the long-lived app-server child. Prompt never appears here.
pub(crate) fn compose_app_server_plan(
    binary: &std::path::Path,
    extra_argv: &[String],
    cwd: &std::path::Path,
    extra_env: &[(String, String)],
    browser: Option<&BrowserChannelSpec>,
) -> Result<LaunchPlan, HarnessError> {
    let mut argv = vec![
        binary.to_string_lossy().into_owned(),
        "app-server".into(),
        "--stdio".into(),
    ];
    argv.extend(extra_argv.iter().cloned());
    if let Some(spec) = browser {
        argv.push("-c".into());
        let bridge = spec.bridge_command().to_string_lossy();
        argv.push(
            format!(
                "mcp_servers.tb-browser={{command=\"{}\",args=[\"browser-mcp\"],env_vars=[\"TIDEBREAK_BROWSER_CAPFILE\"]}}",
                bridge
            )
            .into(),
        );
    }
    let mut env = extra_env.to_vec();
    env.retain(|(key, _)| !BrowserChannelSpec::is_reserved_env_key(key) && key != "PWD");
    let plan = LaunchPlan {
        argv,
        cwd: cwd.to_path_buf(),
        env,
    };
    validate_launch_plan(&plan)?;
    Ok(plan)
}

/// `thread/start` sandbox + approvalPolicy for a permission mode.
#[must_use]
pub(crate) fn thread_start_policy(mode: CodePermissionMode) -> (&'static str, &'static str) {
    match mode {
        CodePermissionMode::Plan => ("read-only", "untrusted"),
        CodePermissionMode::Ask => ("workspace-write", "untrusted"),
        CodePermissionMode::Auto => ("workspace-write", "on-request"),
        CodePermissionMode::Allow => ("danger-full-access", "never"),
    }
}

/// Spawn the app-server child and complete initialize + thread/start|resume.
pub(super) async fn attach(spec: SessionSpec) -> Result<CodexSession, HarnessError> {
    let mut session = CodexSession::new(spec);
    let plan = compose_app_server_plan(
        &session.spec.binary,
        &session.spec.extra_argv,
        &session.spec.worktree,
        &session.spec.extra_env,
        session.spec.browser.as_ref(),
    )?;
    let mut command = Command::new(&plan.argv[0]);
    command
        .args(&plan.argv[1..])
        .current_dir(&plan.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    apply_child_env_tokio(
        &mut command,
        session.spec.env.iter().cloned(),
        &plan.env,
        session.spec.browser.as_ref(),
    );
    let mut child = spawn_process_tree(&mut command)?;
    let stdin = child
        .take_stdin()
        .ok_or_else(|| HarnessError::Other("engine child has no stdin".into()))?;
    let stdout = child
        .take_stdout()
        .ok_or_else(|| HarnessError::Other("engine child has no stdout".into()))?;
    let stderr = child
        .take_stderr()
        .ok_or_else(|| HarnessError::Other("engine child has no stderr".into()))?;
    session.stdin = Some(Arc::new(AsyncMutex::new(stdin)));
    session.stdout = Some(Arc::new(AsyncMutex::new(StdoutReader {
        stdout,
        lines: StreamLineBuffer::new(),
    })));
    session
        .child_pid
        .store(child.id().unwrap_or(0), Ordering::SeqCst);
    *session.child.lock().await = Some(child);
    tokio::spawn(async move {
        let _ = drain_capped(stderr, MAX_STDERR_BYTES).await;
    });

    let init_id = session
        .request(
            "initialize",
            json!({
                "clientInfo": { "name": "tidebreak-harness", "version": "0.0.0" },
                "capabilities": { "experimentalApi": false }
            }),
        )
        .await?;
    session.read_until_rpc(init_id).await?;
    session.notify("initialized", None).await?;

    let (method, params) =
        if let Some(resume) = session.resume_ref.lock().expect("codex resume").clone() {
            ("thread/resume", json!({ "threadId": resume }))
        } else {
            let (sandbox, approval) = thread_start_policy(session.spec.permission_mode);
            let mut params = json!({
                "cwd": session.spec.worktree,
                "approvalPolicy": approval,
                "sandbox": sandbox,
            });
            if let Some(model) = &session.spec.model {
                params["model"] = json!(model);
            }
            ("thread/start", params)
        };
    let thread_req = session.request(method, params).await?;
    session.read_until_rpc(thread_req).await?;
    if let Some(detail) = session.lost_resume() {
        // The stored thread is gone on the engine side. Every turn on this
        // child would fail identically, so fail the launch with a reason the
        // caller can act on instead of attaching a session that cannot run.
        return Err(HarnessError::ResumeLost(detail));
    }
    Ok(session)
}

impl CodexSession {
    async fn read_until_rpc(&self, rpc_id: i64) -> Result<(), HarnessError> {
        let deadline = tokio::time::Instant::now() + HANDSHAKE_TIMEOUT;
        loop {
            if tokio::time::Instant::now() > deadline {
                return Err(HarnessError::Other(format!(
                    "timed out waiting for rpc id {rpc_id}"
                )));
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let lines = timeout(remaining, self.read_lines()).await;
            let lines = match lines {
                Ok(Ok(lines)) => lines,
                Ok(Err(err)) => return Err(err),
                Err(_) => {
                    return Err(HarnessError::Other(format!(
                        "timed out waiting for rpc id {rpc_id}"
                    )));
                }
            };
            let mut seen = false;
            for line in lines {
                if line_is_rpc_id(&line, rpc_id) {
                    seen = true;
                }
                self.emit_parsed(&line).await;
            }
            if seen {
                return Ok(());
            }
        }
    }

    async fn read_until_terminal_turn(&self) -> Result<(), HarnessError> {
        let mut terminal = false;
        loop {
            let changed = self.control_state_changed.notified();
            let deadline = self.next_control_deadline();
            if terminal && !self.controls_pending() {
                return Ok(());
            }

            let lines = match deadline {
                Some(deadline) => {
                    tokio::select! {
                        biased;
                        lines = self.read_lines() => lines?,
                        () = changed => continue,
                        () = tokio::time::sleep_until(deadline) => {
                            self.expire_control_requests();
                            continue;
                        }
                    }
                }
                None => {
                    tokio::select! {
                        biased;
                        lines = self.read_lines() => lines?,
                        () = changed => continue,
                    }
                }
            };
            if lines.is_empty() {
                let message = if terminal {
                    "engine stdout closed before a control response arrived"
                } else {
                    "engine stdout closed before the turn finished"
                };
                return Err(HarnessError::Other(message.into()));
            }
            for line in lines {
                for event in self.emit_parsed(&line).await {
                    if matches!(
                        event,
                        HarnessEvent::TurnCompleted { .. }
                            | HarnessEvent::TurnFailed { .. }
                            | HarnessEvent::TurnInterrupted
                    ) {
                        terminal = true;
                    }
                }
            }
        }
    }

    async fn read_lines(&self) -> Result<Vec<String>, HarnessError> {
        let Some(stdout) = &self.stdout else {
            return Err(HarnessError::Other("engine child has no stdout".into()));
        };
        let mut reader = stdout.lock().await;
        let budget = StreamBudget::default();
        let mut chunk = vec![0_u8; budget.chunk_size];
        loop {
            match reader.stdout.read(&mut chunk).await? {
                0 => return Ok(Vec::new()),
                n => {
                    let tick = reader.lines.push(&chunk[..n], budget);
                    if tick.overflow_chunks > 0 {
                        warn!(
                            overflow_chunks = tick.overflow_chunks,
                            "engine stdout exceeded the parse budget"
                        );
                    }
                    if !tick.lines.is_empty() {
                        return Ok(tick.lines);
                    }
                }
            }
        }
    }

    async fn emit_parsed(&self, line: &str) -> Vec<HarnessEvent> {
        let value = serde_json::from_str::<Value>(line).ok();
        if let Some(value) = value.as_ref() {
            if is_rpc_response(value) {
                if let Some(rpc_id) = value_rpc_id(value) {
                    let pending = self
                        .control_state
                        .lock()
                        .expect("codex control state")
                        .pending
                        .remove(&rpc_id);
                    if let Some(mut pending) = pending {
                        let result = validate_steer_response(value, &pending.expected_turn_id);
                        if pending.accept_response && result.is_ok() {
                            self.spec
                                .sink
                                .emit(HarnessEvent::UserSteered { text: pending.text })
                                .await;
                        }
                        if let Some(reply) = pending.reply.take() {
                            let _ = reply.send(result);
                        }
                        self.control_state_changed.notify_waiters();
                    }
                }
            }
            if let Some(detail) = lost_resume_detail(value) {
                *self.resume_lost.lock().expect("codex resume lost") = Some(detail);
            }
        }
        let events = self.parser.lock().expect("codex parser").push_line(line);
        let terminal = events.iter().any(|event| {
            matches!(
                event,
                HarnessEvent::TurnCompleted { .. }
                    | HarnessEvent::TurnFailed { .. }
                    | HarnessEvent::TurnInterrupted
            )
        });
        if terminal {
            self.close_control_turn_for_terminal(
                "the active turn finished before steering was accepted",
            );
        } else if let Some(turn_id) = value.as_ref().and_then(observed_turn_id) {
            self.activate_control_turn(turn_id);
            // The engine acknowledged a turn on this thread, so it has
            // written the thread's rollout: the id is resumable now.
            self.thread_ran_a_turn.store(true, Ordering::SeqCst);
        }
        for event in &events {
            if let HarnessEvent::SessionStarted {
                resume_ref: Some(resume),
                ..
            } = event
            {
                *self.resume_ref.lock().expect("codex resume") = Some(resume.clone());
            }
            if let HarnessEvent::ApprovalRequested { harness_ref, .. } = event {
                if let Some(id) = self
                    .parser
                    .lock()
                    .expect("codex parser")
                    .pending_approval_rpc_id(&harness_ref.call_id)
                {
                    self.pending_approvals
                        .lock()
                        .expect("codex approvals")
                        .insert(harness_ref.call_id.clone(), id.clone());
                }
                if self.spec.permission_mode == CodePermissionMode::Allow {
                    // Allow is the engine's unsupervised posture. A request
                    // that still arrives must not park a card.
                    let _ = self
                        .decide(harness_ref.clone(), ApprovalDecision::Approve)
                        .await;
                    continue;
                }
            }
            self.spec.sink.emit(event.clone()).await;
        }
        events
    }
}

#[async_trait]
impl HarnessSession for CodexSession {
    async fn run_turn(&self, input: TurnInput) -> Result<TurnOutcome, HarnessError> {
        if self.child_pid.load(Ordering::SeqCst) == 0 {
            return Err(HarnessError::Other("engine child is not running".into()));
        }
        let Some(thread_id) = self.resume_ref.lock().expect("codex resume").clone() else {
            return Err(HarnessError::Other("thread has no resume ref".into()));
        };
        self.begin_control_turn();
        let id = match self
            .request(
                "turn/start",
                json!({
                    "threadId": thread_id,
                    "input": [{ "type": "text", "text": input.text }],
                }),
            )
            .await
        {
            Ok(id) => id,
            Err(err) => {
                self.abort_control_turn("the turn did not start");
                return Err(err);
            }
        };
        let _ = id;
        // Long-lived child: its exit is a session-level failure, not a turn
        // outcome, and `read_until_terminal_turn` already errors on a stream
        // that ends without one.
        let terminal = self.read_until_terminal_turn().await;
        if let Some(detail) = self.lost_resume() {
            // The thread this session attached to is gone. Report the lost
            // resume rather than a turn failure the caller would retry.
            self.abort_control_turn("the engine session was lost");
            return Err(HarnessError::ResumeLost(detail));
        }
        match terminal {
            Ok(()) => {
                self.abort_control_turn("the active turn finished before steering was accepted");
                Ok(TurnOutcome::Clean)
            }
            Err(err) => {
                self.abort_control_turn("the engine stopped before steering was accepted");
                Err(err)
            }
        }
    }

    async fn steer(&self, text: String) -> Result<(), HarnessError> {
        let Some(thread_id) = self.resume_ref.lock().expect("codex resume").clone() else {
            return Err(HarnessError::SteeringRejected(
                "the engine session has no thread id".into(),
            ));
        };
        self.request_steer(thread_id, text).await
    }

    async fn decide(
        &self,
        approval: HarnessApprovalRef,
        decision: ApprovalDecision,
    ) -> Result<(), HarnessError> {
        let rpc_id = self
            .pending_approvals
            .lock()
            .expect("codex approvals")
            .remove(&approval.call_id)
            .or_else(|| {
                self.parser
                    .lock()
                    .expect("codex parser")
                    .take_pending_approval(&approval.call_id)
            })
            .ok_or_else(|| {
                HarnessError::Other(format!(
                    "no parked approval with call_id {}",
                    approval.call_id
                ))
            })?;
        // Captured channel carries accept/decline only — no rejection string.
        let token = match decision {
            ApprovalDecision::Approve => "accept",
            ApprovalDecision::Deny { .. } => "decline",
        };
        self.write_message(&json!({ "id": rpc_id, "result": { "decision": token } }))
            .await?;
        self.spec
            .sink
            .emit(HarnessEvent::ApprovalResolved {
                harness_ref: approval,
                decision,
            })
            .await;
        Ok(())
    }

    async fn interrupt(&self) -> Result<(), HarnessError> {
        let thread_id = self.resume_ref.lock().expect("codex resume").clone();
        let turn_id = self.active_control_turn_id();
        if let (Some(thread_id), Some(turn_id)) = (thread_id, turn_id) {
            let id = self
                .request(
                    "turn/interrupt",
                    json!({ "threadId": thread_id, "turnId": turn_id }),
                )
                .await;
            if id.is_ok() {
                return Ok(());
            }
        }
        let mut slot = self.child.lock().await;
        let Some(child) = slot.as_mut() else {
            return Ok(());
        };
        child.interrupt(Duration::from_secs(2)).await?;
        *slot = None;
        self.child_pid.store(0, Ordering::SeqCst);
        Ok(())
    }

    fn resume_ref(&self) -> Option<String> {
        // Codex 0.147.0 does not persist a thread that never ran a turn:
        // `thread/resume` on such an id answers "thread not found". Report a
        // thread id only once a turn has run on it, so a session whose engine
        // dies before its first turn re-attaches with a fresh `thread/start`
        // instead of resuming a thread the engine never wrote. A ref this
        // session was handed at launch is already persisted state and stays
        // reported as is.
        if self.thread_ran_a_turn.load(Ordering::SeqCst) {
            return self.resume_ref.lock().expect("codex resume").clone();
        }
        self.spec.resume_ref.clone()
    }

    fn child_pid(&self) -> Option<i64> {
        match self.child_pid.load(Ordering::SeqCst) {
            0 => None,
            pid => Some(i64::from(pid)),
        }
    }

    fn unrecognized_events(&self) -> u64 {
        // One long-lived parser per session, so its own count is already
        // cumulative.
        self.parser.lock().expect("codex parser").unrecognized()
    }

    async fn shutdown(self: Box<Self>) -> Result<(), HarnessError> {
        if let Some(mut child) = self.child.lock().await.take() {
            let _ = child.terminate().await;
        }
        Ok(())
    }
}

/// Detail of a JSON-RPC error that says the thread we are on is gone.
///
/// Codex 0.147.0 answers `thread/resume` and `turn/start` for an unknown
/// thread with `thread not found: <id>`. Matching the wording is deliberate:
/// every other engine error is a turn failure, and only this one means the
/// stored resume ref is dead.
fn lost_resume_detail(value: &Value) -> Option<String> {
    let message = value.pointer("/error/message").and_then(Value::as_str)?;
    message
        .to_ascii_lowercase()
        .contains("thread not found")
        .then(|| message.chars().take(MAX_NOTICE_CHARS).collect())
}

fn line_is_rpc_id(line: &str, rpc_id: i64) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return false;
    };
    if !is_rpc_response(&value) {
        return false;
    }
    match value.get("id") {
        Some(Value::Number(n)) => n.as_i64() == Some(rpc_id),
        Some(Value::String(s)) => s.parse::<i64>().ok() == Some(rpc_id),
        _ => false,
    }
}

fn is_rpc_response(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    object.contains_key("id")
        && !object.contains_key("method")
        && (object.contains_key("result") || object.contains_key("error"))
}

fn observed_turn_id(value: &Value) -> Option<&str> {
    let method = value.get("method").and_then(Value::as_str);
    if method == Some("turn/started") {
        return value.pointer("/params/turn/id").and_then(Value::as_str);
    }
    if is_rpc_response(value) {
        return value.pointer("/result/turn/id").and_then(Value::as_str);
    }
    None
}

fn value_rpc_id(value: &Value) -> Option<i64> {
    match value.get("id") {
        Some(Value::Number(number)) => number.as_i64(),
        Some(Value::String(string)) => string.parse::<i64>().ok(),
        _ => None,
    }
}

fn rpc_error_detail(value: &Value) -> Option<String> {
    let message = value.pointer("/error/message").and_then(Value::as_str)?;
    Some(message.chars().take(MAX_NOTICE_CHARS).collect())
}

fn validate_steer_response(value: &Value, expected_turn_id: &str) -> Result<(), HarnessError> {
    if let Some(detail) = rpc_error_detail(value) {
        return Err(HarnessError::SteeringRejected(detail));
    }
    let accepted_turn_id = value
        .pointer("/result/turnId")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            HarnessError::SteeringRejected(
                "the engine returned no turn id for the steering request".into(),
            )
        })?;
    if accepted_turn_id != expected_turn_id {
        return Err(HarnessError::SteeringRejected(format!(
            "the engine acknowledged turn {accepted_turn_id}, not active turn {expected_turn_id}"
        )));
    }
    Ok(())
}

fn steer_request(
    rpc_id: i64,
    thread_id: &str,
    expected_turn_id: &str,
    text: &str,
    client_message_id: &str,
) -> Value {
    json!({
        "id": rpc_id,
        "method": "turn/steer",
        "params": {
            "threadId": thread_id,
            "expectedTurnId": expected_turn_id,
            "input": [{ "type": "text", "text": text }],
            "clientUserMessageId": client_message_id,
        }
    })
}

fn spawn_control_write(
    stdin: Arc<AsyncMutex<ChildStdin>>,
    control_state: Arc<Mutex<ControlState>>,
    changed: Arc<Notify>,
    parser: Arc<Mutex<CodexStreamParser>>,
    rpc_id: i64,
    message: Value,
) {
    tokio::spawn(async move {
        let line = match serde_json::to_vec(&message) {
            Ok(mut line) => {
                line.push(b'\n');
                line
            }
            Err(err) => {
                fail_control_write(
                    &control_state,
                    &changed,
                    &parser,
                    rpc_id,
                    format!("serialize steering rpc: {err}"),
                );
                return;
            }
        };
        let write = timeout(CONTROL_RPC_TIMEOUT, async {
            let mut stdin = stdin.lock().await;
            {
                let mut state = control_state.lock().expect("codex control state");
                let Some(pending) = state.pending.get_mut(&rpc_id) else {
                    return Ok(false);
                };
                if pending.write_state != PendingWriteState::Queued {
                    return Ok(false);
                }
                pending.write_state = PendingWriteState::Writing;
            }
            changed.notify_waiters();
            stdin.write_all(&line).await?;
            stdin.flush().await?;
            Ok::<bool, std::io::Error>(true)
        })
        .await;
        match write {
            Ok(Ok(false)) => return,
            Ok(Ok(true)) => {}
            Ok(Err(err)) => {
                fail_control_write(
                    &control_state,
                    &changed,
                    &parser,
                    rpc_id,
                    format!("write steering rpc: {err}"),
                );
                return;
            }
            Err(_) => {
                fail_control_write(
                    &control_state,
                    &changed,
                    &parser,
                    rpc_id,
                    "timed out writing the steering request".into(),
                );
                return;
            }
        }
        if let Some(pending) = control_state
            .lock()
            .expect("codex control state")
            .pending
            .get_mut(&rpc_id)
        {
            pending.write_state = PendingWriteState::Written;
            pending.deadline = Some(Instant::now() + CONTROL_RPC_TIMEOUT);
        }
        changed.notify_waiters();
    });
}

fn fail_control_write(
    control_state: &Mutex<ControlState>,
    changed: &Notify,
    parser: &Mutex<CodexStreamParser>,
    rpc_id: i64,
    detail: String,
) {
    let pending = control_state
        .lock()
        .expect("codex control state")
        .pending
        .remove(&rpc_id);
    if let Some(mut pending) = pending {
        if let Some(reply) = pending.reply.take() {
            let _ = reply.send(Err(HarnessError::SteeringRejected(detail)));
        }
    }
    parser
        .lock()
        .expect("codex parser")
        .forget_outbound(&json!(rpc_id));
    changed.notify_waiters();
}

async fn drain_capped<R>(mut reader: R, cap: usize) -> String
where
    R: AsyncReadExt + Unpin,
{
    let mut out = Vec::new();
    let mut buf = [0_u8; 4_096];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if out.len() < cap {
                    let room = cap - out.len();
                    out.extend_from_slice(&buf[..n.min(room)]);
                }
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn app_server_plan_is_clean() {
        let plan = compose_app_server_plan(
            std::path::Path::new("/usr/bin/codex"),
            &[],
            std::path::Path::new("/workspace"),
            &[],
            None,
        )
        .unwrap();
        assert_eq!(plan.argv, ["/usr/bin/codex", "app-server", "--stdio"]);
        validate_launch_plan(&plan).unwrap();
    }

    #[test]
    fn extra_bypass_flag_is_rejected() {
        let err = compose_app_server_plan(
            std::path::Path::new("/usr/bin/codex"),
            &["--dangerously-bypass-approvals-and-sandbox".into()],
            std::path::Path::new("/workspace"),
            &[],
            None,
        )
        .unwrap_err();
        assert!(matches!(err, HarnessError::LaunchRejected(_)));
    }

    #[test]
    fn permission_mode_mapping_matches_0033() {
        assert_eq!(
            thread_start_policy(CodePermissionMode::Plan),
            ("read-only", "untrusted")
        );
        assert_eq!(
            thread_start_policy(CodePermissionMode::Ask),
            ("workspace-write", "untrusted")
        );
        assert_eq!(
            thread_start_policy(CodePermissionMode::Auto),
            ("workspace-write", "on-request")
        );
        assert_eq!(
            thread_start_policy(CodePermissionMode::Allow),
            ("danger-full-access", "never")
        );
        let _ = PathBuf::from("/workspace");
    }

    /// A stand-in `codex app-server --stdio` that speaks just enough of the
    /// 0.147.0 protocol to reproduce the resume hazard: it answers
    /// `thread/resume` for an unknown thread the way codex does, and records
    /// every method it was asked for so a test can assert what was on the
    /// wire. Recorded shapes come from `fixtures/codex/0.147.0/`.
    #[cfg(unix)]
    const FAKE_APP_SERVER: &str = r#"#!/bin/sh
while IFS= read -r line; do
  id=${line#*\"id\":}
  id=${id%%,*}
  case "$line" in
    *'"method":"initialize"'*)
      printf 'initialize\n' >>"$FAKE_CODEX_CALLS"
      printf '{"id":%s,"result":{"userAgent":"fake/0.147.0"}}\n' "$id"
      ;;
    *'"method":"thread/start"'*)
      printf 'thread/start\n' >>"$FAKE_CODEX_CALLS"
      printf '{"id":%s,"result":{"thread":{"id":"THREAD-1","cliVersion":"0.147.0","turns":[]}}}\n' "$id"
      ;;
    *'"method":"thread/resume"'*)
      printf 'thread/resume\n' >>"$FAKE_CODEX_CALLS"
      printf '{"id":%s,"error":{"code":-32603,"message":"thread not found: STALE-THREAD"}}\n' "$id"
      ;;
    *'"method":"turn/start"'*)
      printf 'turn/start\n' >>"$FAKE_CODEX_CALLS"
      printf '{"id":%s,"result":{"turn":{"id":"TURN-1","status":"inProgress"}}}\n' "$id"
      printf '{"method":"turn/completed","params":{"threadId":"THREAD-1","turn":{"id":"TURN-1","status":"completed"}}}\n'
      ;;
  esac
done
"#;

    #[cfg(unix)]
    const FAKE_STEERING_APP_SERVER: &str = r#"#!/bin/sh
while IFS= read -r line; do
  id=${line#*\"id\":}
  id=${id%%,*}
  case "$line" in
    *'"method":"initialize"'*)
      printf '{"id":%s,"result":{"userAgent":"fake/0.147.0"}}\n' "$id"
      ;;
    *'"method":"thread/start"'*)
      printf '{"id":%s,"result":{"thread":{"id":"THREAD-1","cliVersion":"0.147.0","turns":[]}}}\n' "$id"
      ;;
    *'"method":"turn/start"'*)
      printf '{"id":%s,"result":{"turn":{"id":"TURN-1","status":"inProgress"}}}\n' "$id"
      printf '{"method":"turn/started","params":{"threadId":"THREAD-1","turn":{"id":"TURN-1","status":"inProgress"}}}\n'
      ;;
    *'"method":"turn/steer"'*)
      printf '%s\n' "$line" >"$FAKE_CODEX_STEER"
      printf '{"id":%s,"result":{"turnId":"TURN-1"}}\n' "$id"
      printf '{"method":"turn/completed","params":{"threadId":"THREAD-1","turn":{"id":"TURN-1","status":"completed"}}}\n'
      ;;
  esac
done
"#;

    #[cfg(unix)]
    fn write_fake_app_server(path: &std::path::Path) {
        write_app_server(path, FAKE_APP_SERVER);
    }

    #[cfg(unix)]
    fn write_app_server(path: &std::path::Path, script: &str) {
        // Write a sibling inode, fsync, then rename over `path` so execve
        // never sees a file that still has a writer (Linux ETXTBSY).
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let staging = path.with_extension("writing");
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o755)
            .open(&staging)
            .unwrap();
        file.write_all(script.as_bytes()).unwrap();
        file.sync_all().unwrap();
        drop(file);
        std::fs::rename(&staging, path).unwrap();
        if let Some(parent) = path.parent() {
            let dir = std::fs::File::open(parent).unwrap();
            dir.sync_all().unwrap();
        }
    }

    #[cfg(unix)]
    struct SilentSink;

    #[cfg(unix)]
    #[async_trait]
    impl crate::HarnessEventSink for SilentSink {
        async fn emit(&self, _event: HarnessEvent) {}
    }

    #[derive(Default)]
    struct RecordingSink {
        events: Mutex<Vec<HarnessEvent>>,
    }

    #[async_trait]
    impl crate::HarnessEventSink for RecordingSink {
        async fn emit(&self, event: HarnessEvent) {
            self.events.lock().expect("codex test events").push(event);
        }
    }

    fn unit_session(sink: Arc<dyn crate::HarnessEventSink>) -> CodexSession {
        CodexSession::new(SessionSpec {
            worktree: PathBuf::from("."),
            permission_mode: CodePermissionMode::Auto,
            model: None,
            resume_ref: Some("THREAD-1".into()),
            extra_argv: Vec::new(),
            extra_env: Vec::new(),
            env: Vec::new(),
            approval: None,
            binary: PathBuf::from("codex"),
            sink,
            browser: None,
        })
    }

    fn register_pending(
        session: &CodexSession,
        rpc_id: i64,
        write_state: PendingWriteState,
        deadline: Option<Instant>,
    ) -> oneshot::Receiver<Result<(), HarnessError>> {
        let (reply, receiver) = oneshot::channel();
        session
            .parser
            .lock()
            .expect("codex parser")
            .note_outbound(&json!(rpc_id), "turn/steer");
        let mut state = session.control_state.lock().expect("codex control state");
        state.turn = ControlTurn::Active("TURN-1".into());
        state.pending.insert(
            rpc_id,
            PendingSteer {
                expected_turn_id: "TURN-1".into(),
                text: "redirect".into(),
                reply: Some(reply),
                deadline,
                write_state,
                accept_response: true,
            },
        );
        receiver
    }

    #[cfg(unix)]
    fn spec_for(
        dir: &std::path::Path,
        binary: &std::path::Path,
        resume_ref: Option<String>,
    ) -> SessionSpec {
        SessionSpec {
            worktree: dir.to_path_buf(),
            permission_mode: CodePermissionMode::Auto,
            model: None,
            resume_ref,
            extra_argv: Vec::new(),
            extra_env: vec![(
                "FAKE_CODEX_CALLS".into(),
                dir.join("calls").to_string_lossy().into_owned(),
            )],
            env: Vec::new(),
            approval: None,
            binary: binary.to_path_buf(),
            sink: Arc::new(SilentSink),
            browser: None,
        }
    }

    #[cfg(unix)]
    fn calls(dir: &std::path::Path) -> Vec<String> {
        std::fs::read_to_string(dir.join("calls"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// The wedge from the app-server dying before its first turn: codex never
    /// persisted the thread, so a thread id that has run no turn must not be
    /// reported as a resume ref. The next attach then starts a clean thread.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_thread_that_ran_no_turn_is_not_a_resume_ref() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("codex");
        write_fake_app_server(&binary);

        let session = attach(spec_for(dir.path(), &binary, None)).await.unwrap();
        assert_eq!(calls(dir.path()), ["initialize", "thread/start"]);
        assert_eq!(
            session.resume_ref(),
            None,
            "a thread with no turns is not resumable and must not be persisted"
        );

        session
            .run_turn(TurnInput {
                text: "first turn".into(),
                model: None,
                images: Vec::new(),
            })
            .await
            .unwrap();
        assert_eq!(session.resume_ref().as_deref(), Some("THREAD-1"));
    }

    /// A resume ref the engine no longer knows is a lost resume, not a turn
    /// failure: the server fences on this rather than failing every turn.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_stale_resume_ref_reports_a_lost_resume() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("codex");
        write_fake_app_server(&binary);

        let attached = attach(spec_for(
            dir.path(),
            &binary,
            Some("STALE-THREAD".to_owned()),
        ))
        .await;
        let Err(err) = attached else {
            panic!("attaching to an unknown thread must not succeed");
        };
        assert_eq!(calls(dir.path()), ["initialize", "thread/resume"]);
        let HarnessError::ResumeLost(detail) = err else {
            panic!("expected a lost resume, got {err}");
        };
        assert!(detail.contains("thread not found"), "detail: {detail}");
    }

    #[test]
    fn steer_response_must_acknowledge_the_expected_turn() {
        validate_steer_response(&json!({ "result": { "turnId": "TURN-1" } }), "TURN-1").unwrap();
        let mismatch =
            validate_steer_response(&json!({ "result": { "turnId": "TURN-2" } }), "TURN-1")
                .unwrap_err();
        assert!(matches!(mismatch, HarnessError::SteeringRejected(_)));
        let rejected = validate_steer_response(
            &json!({ "error": { "message": "turn is no longer steerable" } }),
            "TURN-1",
        )
        .unwrap_err();
        assert!(matches!(rejected, HarnessError::SteeringRejected(_)));
    }

    #[test]
    fn native_steer_request_matches_the_verified_json_shape() {
        let request = steer_request(
            41,
            "THREAD-1",
            "TURN-1",
            "try the other file",
            "tidebreak-steer-41",
        );
        assert_eq!(request["id"], 41);
        assert_eq!(request["method"], "turn/steer");
        assert_eq!(request["params"]["threadId"], "THREAD-1");
        assert_eq!(request["params"]["expectedTurnId"], "TURN-1");
        assert_eq!(
            request["params"]["input"],
            json!([{
                "type": "text",
                "text": "try the other file"
            }])
        );
        assert_eq!(
            request["params"]["clientUserMessageId"],
            "tidebreak-steer-41"
        );
    }

    #[test]
    fn only_true_json_rpc_responses_match_waiters() {
        assert!(!is_rpc_response(&json!({
            "id": 7,
            "method": "item/commandExecution/requestApproval",
            "params": { "itemId": "call-1" }
        })));
        assert!(is_rpc_response(&json!({
            "id": 7,
            "result": { "turnId": "TURN-1" }
        })));
        assert!(is_rpc_response(&json!({
            "id": "7",
            "error": { "code": -32602, "message": "rejected" }
        })));
    }

    #[tokio::test]
    async fn same_id_server_request_does_not_resolve_steering() {
        let sink = Arc::new(RecordingSink::default());
        let session = unit_session(sink.clone());
        let receiver = register_pending(
            &session,
            7,
            PendingWriteState::Written,
            Some(Instant::now() + CONTROL_RPC_TIMEOUT),
        );

        session
            .emit_parsed(
                r#"{"id":7,"method":"item/commandExecution/requestApproval","params":{"itemId":"call-1"}}"#,
            )
            .await;
        assert!(session
            .control_state
            .lock()
            .expect("codex control state")
            .pending
            .contains_key(&7));

        session
            .emit_parsed(r#"{"id":7,"result":{"turnId":"TURN-1"}}"#)
            .await;
        receiver.await.unwrap().unwrap();
        let events = sink.events.lock().expect("codex test events");
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, HarnessEvent::UserSteered { .. }))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn terminal_closes_admission_and_rejects_queued_steering() {
        let session = unit_session(Arc::new(RecordingSink::default()));
        let receiver = register_pending(&session, 7, PendingWriteState::Queued, None);

        session
            .emit_parsed(
                r#"{"method":"turn/completed","params":{"turn":{"id":"TURN-1","status":"completed"}}}"#,
            )
            .await;

        {
            let state = session.control_state.lock().expect("codex control state");
            assert_eq!(state.turn, ControlTurn::Closed);
            assert!(state.pending.is_empty());
        }
        assert!(matches!(
            receiver.await.unwrap(),
            Err(HarnessError::SteeringRejected(_))
        ));
    }

    #[tokio::test]
    async fn terminal_race_consumes_an_inflight_ack_without_a_steer_event() {
        let sink = Arc::new(RecordingSink::default());
        let session = unit_session(sink.clone());
        let receiver = register_pending(
            &session,
            7,
            PendingWriteState::Writing,
            Some(Instant::now() + CONTROL_RPC_TIMEOUT),
        );

        session
            .emit_parsed(
                r#"{"method":"turn/completed","params":{"turn":{"id":"TURN-1","status":"completed"}}}"#,
            )
            .await;
        assert!(matches!(
            receiver.await.unwrap(),
            Err(HarnessError::SteeringRejected(_))
        ));
        assert!(session
            .control_state
            .lock()
            .expect("codex control state")
            .pending
            .contains_key(&7));

        session
            .emit_parsed(r#"{"id":7,"result":{"turnId":"TURN-1"}}"#)
            .await;
        assert!(session
            .control_state
            .lock()
            .expect("codex control state")
            .pending
            .is_empty());
        assert!(!sink
            .events
            .lock()
            .expect("codex test events")
            .iter()
            .any(|event| matches!(event, HarnessEvent::UserSteered { .. })));
    }

    #[test]
    fn cancellation_before_write_removes_the_registration() {
        let session = unit_session(Arc::new(RecordingSink::default()));
        let receiver = register_pending(&session, 7, PendingWriteState::Queued, None);
        drop(receiver);
        {
            let _registration = ControlRegistration {
                session: &session,
                rpc_id: 7,
                armed: true,
            };
        }
        assert!(session
            .control_state
            .lock()
            .expect("codex control state")
            .pending
            .is_empty());
    }

    #[tokio::test]
    async fn caller_cancellation_after_native_acceptance_keeps_user_steered() {
        let sink = Arc::new(RecordingSink::default());
        let session = unit_session(sink.clone());
        let receiver = register_pending(
            &session,
            7,
            PendingWriteState::Written,
            Some(Instant::now() + CONTROL_RPC_TIMEOUT),
        );
        drop(receiver);

        session
            .emit_parsed(r#"{"id":7,"result":{"turnId":"TURN-1"}}"#)
            .await;

        let events = sink.events.lock().expect("codex test events");
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, HarnessEvent::UserSteered { .. }))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn user_steered_is_emitted_before_turn_completed() {
        let sink = Arc::new(RecordingSink::default());
        let session = unit_session(sink.clone());
        let receiver = register_pending(
            &session,
            7,
            PendingWriteState::Written,
            Some(Instant::now() + CONTROL_RPC_TIMEOUT),
        );

        session
            .emit_parsed(r#"{"id":7,"result":{"turnId":"TURN-1"}}"#)
            .await;
        session
            .emit_parsed(
                r#"{"method":"turn/completed","params":{"turn":{"id":"TURN-1","status":"completed"}}}"#,
            )
            .await;
        receiver.await.unwrap().unwrap();

        let events = sink.events.lock().expect("codex test events");
        let steered = events
            .iter()
            .position(|event| matches!(event, HarnessEvent::UserSteered { .. }))
            .unwrap();
        let completed = events
            .iter()
            .position(|event| matches!(event, HarnessEvent::TurnCompleted { .. }))
            .unwrap();
        assert!(steered < completed);
    }

    #[tokio::test]
    async fn rejected_or_mismatched_ack_never_emits_user_steered() {
        for response in [
            json!({ "id": 7, "error": { "message": "not steerable" } }),
            json!({ "id": 7, "result": { "turnId": "TURN-2" } }),
        ] {
            let sink = Arc::new(RecordingSink::default());
            let session = unit_session(sink.clone());
            let receiver = register_pending(
                &session,
                7,
                PendingWriteState::Written,
                Some(Instant::now() + CONTROL_RPC_TIMEOUT),
            );
            session.emit_parsed(&response.to_string()).await;
            assert!(matches!(
                receiver.await.unwrap(),
                Err(HarnessError::SteeringRejected(_))
            ));
            assert!(!sink
                .events
                .lock()
                .expect("codex test events")
                .iter()
                .any(|event| matches!(event, HarnessEvent::UserSteered { .. })));
        }
    }

    #[tokio::test]
    async fn control_timeout_cleans_pending_state() {
        let session = unit_session(Arc::new(RecordingSink::default()));
        let receiver = register_pending(
            &session,
            7,
            PendingWriteState::Written,
            Some(Instant::now()),
        );
        session.expire_control_requests();
        assert!(session
            .control_state
            .lock()
            .expect("codex control state")
            .pending
            .is_empty());
        assert!(matches!(
            receiver.await.unwrap(),
            Err(HarnessError::SteeringRejected(_))
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn native_steer_uses_the_active_turn_id_and_waits_for_ack() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("codex");
        write_app_server(&binary, FAKE_STEERING_APP_SERVER);
        let mut spec = spec_for(dir.path(), &binary, None);
        let sink = Arc::new(RecordingSink::default());
        spec.sink = sink.clone();
        spec.extra_env.push((
            "FAKE_CODEX_STEER".into(),
            dir.path().join("steer.json").to_string_lossy().into_owned(),
        ));
        let session = Arc::new(attach(spec).await.unwrap());
        let running = tokio::spawn({
            let session = Arc::clone(&session);
            async move {
                session
                    .run_turn(TurnInput {
                        text: "first turn".into(),
                        model: None,
                        images: Vec::new(),
                    })
                    .await
            }
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if session.active_control_turn_id().is_some() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("fake turn was never acknowledged");

        session.steer("try the other file".into()).await.unwrap();
        running.await.unwrap().unwrap();

        let request: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.path().join("steer.json")).unwrap())
                .unwrap();
        assert_eq!(request["method"], "turn/steer");
        assert_eq!(request["params"]["threadId"], "THREAD-1");
        assert_eq!(request["params"]["expectedTurnId"], "TURN-1");
        assert_eq!(request["params"]["input"][0]["type"], "text");
        assert_eq!(request["params"]["input"][0]["text"], "try the other file");
        let steers: Vec<_> = sink
            .events
            .lock()
            .expect("codex test events")
            .iter()
            .filter_map(|event| match event {
                HarnessEvent::UserSteered { text } => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(steers, ["try the other file"]);
    }

    // ── Browser MCP advertisement contract tests ──

    #[test]
    fn browser_absent_produces_same_argv_as_before() {
        let plan = compose_app_server_plan(
            std::path::Path::new("/usr/bin/codex"),
            &[],
            std::path::Path::new("/workspace"),
            &[],
            None,
        )
        .unwrap();
        assert_eq!(plan.argv, ["/usr/bin/codex", "app-server", "--stdio"]);
    }

    #[test]
    fn browser_present_appends_exactly_one_trusted_config_override() {
        let spec = BrowserChannelSpec::new(
            std::path::PathBuf::from("/tmp/browser-cap.json"),
            std::path::PathBuf::from("/usr/local/bin/tidebreak"),
        );
        let plan = compose_app_server_plan(
            std::path::Path::new("/usr/bin/codex"),
            &[],
            std::path::Path::new("/workspace"),
            &[],
            Some(&spec),
        )
        .unwrap();
        let overrides: Vec<_> = plan
            .argv
            .iter()
            .enumerate()
            .filter(|(_, arg)| *arg == "-c")
            .collect();
        assert_eq!(overrides.len(), 1);
        let idx = overrides[0].0;
        let value = &plan.argv[idx + 1];
        assert!(value.contains("mcp_servers.tb-browser"));
        assert!(value.contains("command=\"/usr/local/bin/tidebreak\""));
        assert!(value.contains(r#"args=["browser-mcp"]"#));
        assert!(value.contains(r#"env_vars=["TIDEBREAK_BROWSER_CAPFILE"]"#));
        // The override names the env var but must not contain a capfile path or token value.
        let capfile_str = spec.capability_file.to_string_lossy();
        assert!(!value.contains(capfile_str.as_ref()));
    }

    #[test]
    fn browser_override_is_after_extra_argv() {
        let spec = BrowserChannelSpec::new(
            std::path::PathBuf::from("/tmp/browser-cap.json"),
            std::path::PathBuf::from("/usr/local/bin/tidebreak"),
        );
        let plan = compose_app_server_plan(
            std::path::Path::new("/usr/bin/codex"),
            &["--extra".into(), "--flag".into()],
            std::path::Path::new("/workspace"),
            &[],
            Some(&spec),
        )
        .unwrap();
        let browser_idx = plan.argv.iter().position(|arg| arg == "-c").unwrap();
        let extra_flag_idx = plan.argv.iter().position(|arg| arg == "--extra").unwrap();
        assert!(extra_flag_idx < browser_idx);
    }

    #[test]
    fn browser_capfile_path_is_never_in_argv() {
        let capfile = std::path::PathBuf::from("/tmp/tidebreak-browser-abc123.json");
        let spec = BrowserChannelSpec::new(
            capfile.clone(),
            std::path::PathBuf::from("/usr/local/bin/tidebreak"),
        );
        let plan = compose_app_server_plan(
            std::path::Path::new("/usr/bin/codex"),
            &[],
            std::path::Path::new("/workspace"),
            &[],
            Some(&spec),
        )
        .unwrap();
        let capfile_str = capfile.to_string_lossy();
        assert!(!plan
            .argv
            .iter()
            .any(|arg| arg.contains(capfile_str.as_ref())));
    }

    #[test]
    fn browser_env_key_is_stripped_from_plan_even_when_browser_is_some() {
        let spec = BrowserChannelSpec::new(
            std::path::PathBuf::from("/tmp/browser-cap.json"),
            std::path::PathBuf::from("/usr/local/bin/tidebreak"),
        );
        let plan = compose_app_server_plan(
            std::path::Path::new("/usr/bin/codex"),
            &[],
            std::path::Path::new("/workspace"),
            &[("TIDEBREAK_BROWSER_CAPFILE".into(), "/evil/cap.json".into())],
            Some(&spec),
        )
        .unwrap();
        let has_reserved = plan
            .env
            .iter()
            .any(|(key, _)| BrowserChannelSpec::is_reserved_env_key(key));
        assert!(!has_reserved);
    }

    #[test]
    fn bridge_command_with_spaces_remains_one_command_value() {
        let spec = BrowserChannelSpec::new(
            std::path::PathBuf::from("/tmp/browser-cap.json"),
            std::path::PathBuf::from("/Applications/Tidebreak.app/Contents/bin/tidebreak"),
        );
        let plan = compose_app_server_plan(
            std::path::Path::new("/usr/bin/codex"),
            &[],
            std::path::Path::new("/workspace"),
            &[],
            Some(&spec),
        )
        .unwrap();
        let override_idx = plan.argv.iter().position(|arg| arg == "-c").unwrap();
        let value = &plan.argv[override_idx + 1];
        // The command must appear as one quoted value, not split on spaces.
        assert!(value.contains("command=\"/Applications/Tidebreak.app/Contents/bin/tidebreak\""));
        // args must still be exactly ["browser-mcp"].
        assert!(value.contains(r#"args=["browser-mcp"]"#));
    }
}
