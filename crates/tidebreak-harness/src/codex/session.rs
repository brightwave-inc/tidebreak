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
use tidebreak_core::{PermissionMode, ReasoningEffort, MAX_NOTICE_CHARS};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);
const THREAD_LOAD_TIMEOUT: Duration = Duration::from_secs(120);
const THREAD_LOAD_ABSOLUTE_CEILING: Duration = Duration::from_secs(30 * 60);
#[cfg(not(test))]
const PROCESS_INTERRUPT_GRACE: Duration = Duration::from_secs(2);
#[cfg(test)]
const PROCESS_INTERRUPT_GRACE: Duration = Duration::from_millis(50);
#[cfg(not(test))]
const CONTROL_RPC_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(test)]
const CONTROL_RPC_TIMEOUT: Duration = Duration::from_millis(250);
const MAX_STDERR_BYTES: usize = 64 * 1_024;

/// The service-tier id Codex serves fast mode under.
///
/// Its catalog advertises the tier two ways — `additionalSpeedTiers` lists it
/// as `fast`, while `serviceTiers` carries the id a request sends. The request
/// takes the latter.
const FAST_SERVICE_TIER: &str = "priority";

/// Live Codex session: one app-server child, spawned on the first turn and
/// replaced whenever a turn finds it parked or dead (decision 0064).
pub struct CodexSession {
    spec: SessionSpec,
    /// The session's current permission mode. `turn/start` re-postures a
    /// thread for "this turn and subsequent turns", so a switch lands here and
    /// rides out on the next turn rather than relaunching the child.
    permission_mode: Mutex<PermissionMode>,
    /// Posture generations that still need correlated engine admission.
    posture: Mutex<PostureState>,
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
    stdin: Mutex<Option<Arc<AsyncMutex<ChildStdin>>>>,
    stdout: Mutex<Option<Arc<AsyncMutex<StdoutReader>>>>,
    parser: Arc<Mutex<CodexStreamParser>>,
    next_id: AtomicI64,
    interrupts_this_turn: AtomicU32,
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
    interrupt: Option<PendingInterrupt>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PostureGeneration {
    id: u64,
    mode: PermissionMode,
}

struct PostureState {
    next_generation: u64,
    pending: Option<PostureGeneration>,
    admission: Option<TurnAdmission>,
}

struct TurnAdmission {
    rpc_id: i64,
    thread_id: String,
    posture_generation: Option<u64>,
    turn_id: Option<String>,
    deadline: Instant,
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

struct PendingInterrupt {
    rpc_id: i64,
    reply: Option<oneshot::Sender<Result<(), HarnessError>>>,
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
        let permission_mode = spec.permission_mode;
        let pending_posture = resume_ref.as_ref().map(|_| PostureGeneration {
            id: 1,
            mode: permission_mode,
        });
        Self {
            spec,
            permission_mode: Mutex::new(permission_mode),
            posture: Mutex::new(PostureState {
                next_generation: u64::from(pending_posture.is_some()),
                pending: pending_posture,
                admission: None,
            }),
            resume_ref: Mutex::new(resume_ref),
            thread_ran_a_turn: AtomicBool::new(false),
            resume_lost: Mutex::new(None),
            child: AsyncMutex::new(None),
            child_pid: AtomicU32::new(0),
            stdin: Mutex::new(None),
            stdout: Mutex::new(None),
            parser: Arc::new(Mutex::new(CodexStreamParser::new())),
            next_id: AtomicI64::new(1),
            interrupts_this_turn: AtomicU32::new(0),
            control_state: Arc::new(Mutex::new(ControlState {
                turn: ControlTurn::Idle,
                pending: HashMap::new(),
                interrupt: None,
            })),
            control_state_changed: Arc::new(Notify::new()),
            pending_approvals: Mutex::new(HashMap::new()),
        }
    }

    /// The mode in force right now.
    fn permission_mode(&self) -> PermissionMode {
        *self.permission_mode.lock().expect("codex permission mode")
    }

    /// The effort a turn runs at: the turn's own, else the session's.
    ///
    /// Not clamped here, unlike the adapters with a fixed ladder. Codex states
    /// its ladder per model on `model/list` and validates the string against
    /// that row, so the ladder this session's model takes is not something the
    /// session knows — the picker offers only advertised rungs, and the engine
    /// itself refuses one it does not.
    fn resolved_effort(&self, turn_effort: Option<ReasoningEffort>) -> Option<ReasoningEffort> {
        turn_effort.or(self.spec.reasoning_effort)
    }

    /// The detail of a lost resume observed on the stream, when any.
    fn lost_resume(&self) -> Option<String> {
        self.resume_lost.lock().expect("codex resume lost").clone()
    }

    fn next_rpc_id(&self) -> i64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    fn arm_posture(&self, mode: PermissionMode) -> PostureGeneration {
        let mut state = self.posture.lock().expect("codex posture");
        state.next_generation = state.next_generation.saturating_add(1);
        let generation = PostureGeneration {
            id: state.next_generation,
            mode,
        };
        state.pending = Some(generation);
        generation
    }

    fn ensure_posture_pending(&self) -> PostureGeneration {
        let mode = self.permission_mode();
        let mut state = self.posture.lock().expect("codex posture");
        if let Some(generation) = state.pending {
            return generation;
        }
        state.next_generation = state.next_generation.saturating_add(1);
        let generation = PostureGeneration {
            id: state.next_generation,
            mode,
        };
        state.pending = Some(generation);
        generation
    }

    fn pending_posture(&self) -> Option<PostureGeneration> {
        self.posture.lock().expect("codex posture").pending
    }

    fn register_turn_admission(
        &self,
        rpc_id: i64,
        thread_id: String,
        posture_generation: Option<u64>,
    ) {
        self.posture.lock().expect("codex posture").admission = Some(TurnAdmission {
            rpc_id,
            thread_id,
            posture_generation,
            turn_id: None,
            deadline: Instant::now() + HANDSHAKE_TIMEOUT,
        });
    }

    fn turn_admission_deadline(&self) -> Option<Instant> {
        self.posture
            .lock()
            .expect("codex posture")
            .admission
            .as_ref()
            .map(|admission| admission.deadline)
    }

    fn expire_turn_admission(&self) -> bool {
        let mut state = self.posture.lock().expect("codex posture");
        if state
            .admission
            .as_ref()
            .is_some_and(|admission| admission.deadline <= Instant::now())
        {
            state.admission = None;
            return true;
        }
        false
    }

    fn clear_turn_admission(&self) {
        self.posture.lock().expect("codex posture").admission = None;
    }

    /// Admit only the posture generation that rode on this exact turn.
    /// A newer mode switch leaves a newer generation pending.
    fn observe_turn_admission(&self, value: &Value) -> Option<String> {
        let mut state = self.posture.lock().expect("codex posture");
        let admission = state.admission.as_mut()?;

        let turn_id = if is_rpc_response(value) {
            if value_rpc_id(value) != Some(admission.rpc_id) {
                return None;
            }
            if value.get("error").is_some() {
                state.admission = None;
                return None;
            }
            let turn_id = value.pointer("/result/turn/id").and_then(Value::as_str)?;
            admission.turn_id = Some(turn_id.to_owned());
            turn_id.to_owned()
        } else if value.get("method").and_then(Value::as_str) == Some("turn/started") {
            if value.pointer("/params/threadId").and_then(Value::as_str)
                != Some(admission.thread_id.as_str())
            {
                return None;
            }
            let turn_id = value.pointer("/params/turn/id").and_then(Value::as_str)?;
            if admission
                .turn_id
                .as_deref()
                .is_some_and(|expected| expected != turn_id)
            {
                return None;
            }
            turn_id.to_owned()
        } else {
            return None;
        };

        let generation = admission.posture_generation;
        state.admission = None;
        if generation.is_some_and(|generation| {
            state
                .pending
                .is_some_and(|pending| pending.id == generation)
        }) {
            state.pending = None;
        }
        Some(turn_id)
    }

    async fn write_message(&self, message: &Value) -> Result<(), HarnessError> {
        let Some(stdin) = self.stdin.lock().expect("codex stdin").clone() else {
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
        if let Err(err) = self
            .write_message(&json!({ "id": id, "method": method, "params": params }))
            .await
        {
            self.parser
                .lock()
                .expect("codex parser")
                .forget_outbound(&json!(id));
            return Err(err);
        }
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
        let Some(stdin) = self.stdin.lock().expect("codex stdin").clone() else {
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
        self.interrupts_this_turn.store(0, Ordering::SeqCst);
        let (pending, interrupt) = {
            let mut state = self.control_state.lock().expect("codex control state");
            state.turn = ControlTurn::Starting;
            (std::mem::take(&mut state.pending), state.interrupt.take())
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
        if let Some(mut interrupt) = interrupt {
            if let Some(reply) = interrupt.reply.take() {
                let _ = reply.send(Err(HarnessError::Other(
                    "the prior turn ended before the interrupt was acknowledged".into(),
                )));
            }
            self.parser
                .lock()
                .expect("codex parser")
                .forget_outbound(&json!(interrupt.rpc_id));
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
    fn close_control_turn_for_terminal(&self, detail: &str, interrupted: bool) {
        let (removed, interrupt) = {
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
            (removed, state.interrupt.take())
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
        if let Some(mut pending) = interrupt {
            if let Some(reply) = pending.reply.take() {
                let result = if interrupted {
                    Ok(())
                } else {
                    Err(HarnessError::Other(detail.into()))
                };
                let _ = reply.send(result);
            }
            self.parser
                .lock()
                .expect("codex parser")
                .forget_outbound(&json!(pending.rpc_id));
        }
        self.control_state_changed.notify_waiters();
    }

    fn abort_control_turn(&self, detail: &str) {
        let (pending, interrupt) = {
            let mut state = self.control_state.lock().expect("codex control state");
            state.turn = ControlTurn::Closed;
            (std::mem::take(&mut state.pending), state.interrupt.take())
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
        if let Some(mut pending) = interrupt {
            if let Some(reply) = pending.reply.take() {
                let _ = reply.send(Err(HarnessError::Other(detail.into())));
            }
            self.parser
                .lock()
                .expect("codex parser")
                .forget_outbound(&json!(pending.rpc_id));
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
        let state = self.control_state.lock().expect("codex control state");
        !state.pending.is_empty() || state.interrupt.is_some()
    }

    fn active_control_turn_id(&self) -> Option<String> {
        let state = self.control_state.lock().expect("codex control state");
        match &state.turn {
            ControlTurn::Active(turn_id) => Some(turn_id.clone()),
            ControlTurn::Idle | ControlTurn::Starting | ControlTurn::Closed => None,
        }
    }

    fn register_interrupt(&self, rpc_id: i64) -> oneshot::Receiver<Result<(), HarnessError>> {
        let (reply, receiver) = oneshot::channel();
        self.parser
            .lock()
            .expect("codex parser")
            .note_outbound(&json!(rpc_id), "turn/interrupt");
        self.control_state
            .lock()
            .expect("codex control state")
            .interrupt = Some(PendingInterrupt {
            rpc_id,
            reply: Some(reply),
        });
        receiver
    }

    fn resolve_interrupt_response(&self, rpc_id: i64, value: &Value) -> bool {
        let pending = {
            let mut state = self.control_state.lock().expect("codex control state");
            if state
                .interrupt
                .as_ref()
                .is_none_or(|pending| pending.rpc_id != rpc_id)
            {
                return false;
            }
            state.interrupt.take()
        };
        let Some(mut pending) = pending else {
            return false;
        };
        let result = if let Some(detail) = rpc_error_detail(value) {
            Err(HarnessError::Other(format!(
                "the engine rejected the interrupt: {detail}"
            )))
        } else if value.get("result").is_some() {
            Ok(())
        } else {
            Err(HarnessError::Other(
                "the engine returned a malformed interrupt response".into(),
            ))
        };
        if let Some(reply) = pending.reply.take() {
            let _ = reply.send(result);
        }
        true
    }

    fn cancel_interrupt(&self, rpc_id: i64, detail: &str) {
        let pending = {
            let mut state = self.control_state.lock().expect("codex control state");
            if state
                .interrupt
                .as_ref()
                .is_none_or(|pending| pending.rpc_id != rpc_id)
            {
                return;
            }
            state.interrupt.take()
        };
        if let Some(mut pending) = pending {
            if let Some(reply) = pending.reply.take() {
                let _ = reply.send(Err(HarnessError::Other(detail.into())));
            }
        }
        self.parser
            .lock()
            .expect("codex parser")
            .forget_outbound(&json!(rpc_id));
        self.control_state_changed.notify_waiters();
    }

    fn fail_pending_interrupt(&self, detail: &str) {
        let rpc_id = self
            .control_state
            .lock()
            .expect("codex control state")
            .interrupt
            .as_ref()
            .map(|pending| pending.rpc_id);
        if let Some(rpc_id) = rpc_id {
            self.cancel_interrupt(rpc_id, detail);
        }
    }

    async fn interrupt_process_tree(&self) -> Result<(), HarnessError> {
        self.clear_turn_admission();
        self.fail_pending_interrupt("the native interrupt did not complete");
        let mut slot = self.child.lock().await;
        *self.stdin.lock().expect("codex stdin") = None;
        *self.stdout.lock().expect("codex stdout") = None;
        self.child_pid.store(0, Ordering::SeqCst);
        if let Some(mut child) = slot.take() {
            child.interrupt(PROCESS_INTERRUPT_GRACE).await?;
        }
        Ok(())
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
///
/// `relay_key_env` is the one reserved-namespace variable allowed to reach
/// the child: the provider config the spawn wiring emits reads the session
/// relay key through `env_key={relay_key_env}`, so stripping it would leave
/// a hosted session with no credential at all.
pub(crate) fn compose_app_server_plan(
    binary: &std::path::Path,
    extra_argv: &[String],
    cwd: &std::path::Path,
    extra_env: &[(String, String)],
    browser: Option<&BrowserChannelSpec>,
    relay_key_env: Option<&str>,
) -> Result<LaunchPlan, HarnessError> {
    let mut argv = vec![
        binary.to_string_lossy().into_owned(),
        "app-server".into(),
        "--stdio".into(),
    ];
    argv.extend(extra_argv.iter().cloned());
    if let Some(spec) = browser {
        argv.push("-c".into());
        // Use serde_json to escape the bridge path: backslashes, quotes,
        // and control characters in a Windows or unusual path are turned
        // into valid JSON string characters. Reject non-UTF-8 paths
        // explicitly rather than silently replacing characters.
        let bridge_path = spec.bridge_command().to_str().ok_or_else(|| {
            HarnessError::Other(format!(
                "browser bridge command path is not valid UTF-8: {}",
                spec.bridge_command().display()
            ))
        })?;
        let escaped = serde_json::to_string(bridge_path)
            .expect("serializing a valid &str to JSON cannot fail");
        argv.push(
            format!("mcp_servers.tb-browser={{command={escaped},args=[\"browser-mcp\"],env_vars=[\"TIDEBREAK_BROWSER_CAPFILE\"]}}"),
        );
    }
    let mut env = extra_env.to_vec();
    env.retain(|(key, _)| {
        !BrowserChannelSpec::is_reserved_env_key_except(key, relay_key_env) && key != "PWD"
    });
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
pub(crate) fn thread_start_policy(mode: PermissionMode) -> (&'static str, &'static str) {
    match mode {
        PermissionMode::Plan => ("read-only", "untrusted"),
        PermissionMode::Ask => ("workspace-write", "untrusted"),
        PermissionMode::Auto => ("workspace-write", "on-request"),
        PermissionMode::Allow => ("danger-full-access", "never"),
    }
}

/// The same posture as [`thread_start_policy`], in the shape `turn/start`
/// takes: `sandboxPolicy` is a tagged object there, not the plain mode string
/// `thread/start` accepts. Both fields apply to this turn and every later one,
/// which is what lets a mode switch land without a new child.
#[must_use]
pub(crate) fn turn_start_policy(mode: PermissionMode) -> (Value, &'static str) {
    let (sandbox, approval) = thread_start_policy(mode);
    let sandbox = match sandbox {
        "read-only" => json!({ "type": "readOnly" }),
        "danger-full-access" => json!({ "type": "dangerFullAccess" }),
        _ => json!({ "type": "workspaceWrite" }),
    };
    (sandbox, approval)
}

impl CodexSession {
    /// A live child, spawned and handshaken if there is none (decision 0064).
    ///
    /// The fast path is a running child: nothing to do. Otherwise a parked or
    /// dead child is replaced and the engine session resumed before the
    /// turn's own request goes out. The child slot is held across the whole
    /// ensure, so two callers cannot race two spawns.
    pub(super) async fn ensure_child(&self) -> Result<(), HarnessError> {
        let mut slot = self.child.lock().await;
        if let Some(child) = slot.as_mut() {
            if matches!(child.try_wait(), Ok(None)) {
                return Ok(());
            }
            // The process is gone; drop the handles that pointed at it.
            *slot = None;
            self.child_pid.store(0, Ordering::SeqCst);
            *self.stdin.lock().expect("codex stdin") = None;
            *self.stdout.lock().expect("codex stdout") = None;
        }
        let plan = compose_app_server_plan(
            self.spec.binary.as_deref().ok_or(HarnessError::NotFound)?,
            &self.spec.extra_argv,
            &self.spec.worktree,
            &self.spec.extra_env,
            self.spec.browser.as_ref(),
            self.spec.relay_key_env.as_deref(),
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
            tidebreak_core::HarnessKind::Codex,
            self.spec.env.iter().cloned(),
            &plan.env,
            self.spec.browser.as_ref(),
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
        *self.stdin.lock().expect("codex stdin") = Some(Arc::new(AsyncMutex::new(stdin)));
        *self.stdout.lock().expect("codex stdout") =
            Some(Arc::new(AsyncMutex::new(StdoutReader {
                stdout,
                lines: StreamLineBuffer::new(),
            })));
        self.child_pid
            .store(child.id().unwrap_or(0), Ordering::SeqCst);
        *slot = Some(child);
        tokio::spawn(async move {
            let _ = drain_capped(stderr, MAX_STDERR_BYTES).await;
        });
        if let Err(err) = self.handshake().await {
            // Leave nothing half-attached: the next ensure starts clean, and
            // a fenced session does not keep a wedged server alive until it
            // is reaped.
            if let Some(mut child) = slot.take() {
                let _ = child.terminate().await;
            }
            self.child_pid.store(0, Ordering::SeqCst);
            *self.stdin.lock().expect("codex stdin") = None;
            *self.stdout.lock().expect("codex stdout") = None;
            return Err(err);
        }
        Ok(())
    }

    /// initialize + thread/start|resume on a freshly spawned child.
    async fn handshake(&self) -> Result<(), HarnessError> {
        let init_id = self
            .request(
                "initialize",
                json!({
                    "clientInfo": { "name": "tidebreak-harness", "version": "0.0.0" },
                    "capabilities": { "experimentalApi": false }
                }),
            )
            .await?;
        self.read_until_rpc(init_id, HANDSHAKE_TIMEOUT).await?;
        self.notify("initialized", None).await?;

        // Resume only a thread the engine has actually written. A thread that
        // ran no turn was never persisted (see `resume_ref`), so a respawn
        // after parking one starts a clean thread rather than fencing the
        // session on "thread not found".
        let resume = if self.thread_ran_a_turn.load(Ordering::SeqCst) {
            self.resume_ref.lock().expect("codex resume").clone()
        } else {
            self.spec.resume_ref.clone()
        };
        let (method, params) = if let Some(resume) = resume {
            ("thread/resume", json!({ "threadId": resume }))
        } else {
            let (sandbox, approval) = thread_start_policy(self.permission_mode());
            let mut params = json!({
                "cwd": self.spec.worktree,
                "approvalPolicy": approval,
                "sandbox": sandbox,
            });
            if let Some(model) = &self.spec.model {
                params["model"] = json!(model);
            }
            ("thread/start", params)
        };
        let resumed = method == "thread/resume";
        let thread_req = self.request(method, params).await?;
        self.read_until_rpc(thread_req, THREAD_LOAD_TIMEOUT).await?;
        if let Some(detail) = self.lost_resume() {
            // The stored thread is gone on the engine side. Every turn on
            // this child would fail identically, so report the lost resume
            // for the caller to fence on instead of running a turn that
            // cannot land.
            return Err(HarnessError::ResumeLost(detail));
        }
        // A resumed thread carries whatever posture it was last told, so the
        // next turn restates the mode rather than assume it holds. A started
        // thread was just created with this mode.
        if resumed {
            self.ensure_posture_pending();
        }
        Ok(())
    }

    async fn read_until_rpc(
        &self,
        rpc_id: i64,
        response_timeout: Duration,
    ) -> Result<(), HarnessError> {
        // Loading a large persisted thread can stream more history than
        // fits inside one fixed wall-clock deadline. Treat the bound as
        // an inactivity timeout instead: every batch proves the engine is
        // still making progress, while a silent or wedged child remains
        // bounded. The absolute ceiling still caps a restore that never
        // finishes even though it keeps talking.
        let absolute = Instant::now() + THREAD_LOAD_ABSOLUTE_CEILING;
        loop {
            let remaining = absolute.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(HarnessError::Other(format!(
                    "timed out waiting for rpc id {rpc_id}"
                )));
            }
            let lines = timeout(response_timeout.min(remaining), self.read_lines()).await;
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
            let control_deadline = self.next_control_deadline();
            let admission_deadline = self.turn_admission_deadline();
            let deadline = match (control_deadline, admission_deadline) {
                (Some(control), Some(admission)) => Some(control.min(admission)),
                (Some(deadline), None) | (None, Some(deadline)) => Some(deadline),
                (None, None) => None,
            };
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
                            if self.expire_turn_admission() {
                                return Err(HarnessError::Other(
                                    "timed out waiting for the engine to admit the turn".into(),
                                ));
                            }
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
                self.clear_turn_admission();
                self.fail_pending_interrupt(
                    "engine stdout closed before the interrupt was acknowledged",
                );
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
        let Some(stdout) = self.stdout.lock().expect("codex stdout").clone() else {
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
        let mut admitted_turn_id = None;
        if let Some(value) = value.as_ref() {
            if is_rpc_response(value) {
                if let Some(rpc_id) = value_rpc_id(value) {
                    if self.resolve_interrupt_response(rpc_id, value) {
                        self.control_state_changed.notify_waiters();
                    }
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
            admitted_turn_id = self.observe_turn_admission(value);
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
            self.clear_turn_admission();
            let interrupted = events
                .iter()
                .any(|event| matches!(event, HarnessEvent::TurnInterrupted));
            self.close_control_turn_for_terminal(
                "the active turn finished before steering was accepted",
                interrupted,
            );
        } else if let Some(turn_id) = admitted_turn_id.as_deref() {
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
                if self.spec.permission_mode == PermissionMode::Allow {
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
        self.ensure_child().await?;
        let Some(thread_id) = self.resume_ref.lock().expect("codex resume").clone() else {
            return Err(HarnessError::Other("thread has no resume ref".into()));
        };
        self.begin_control_turn();
        let mut params = json!({
            "threadId": thread_id,
            "input": [{ "type": "text", "text": input.text }],
        });
        // Every override below applies to this turn and the ones after it, so
        // a switch between turns needs no new child. State them only when they
        // moved: an unchanged field re-sent each turn is noise the engine has
        // to reconcile against the thread it already holds.
        if let Some(model) = &input.model {
            params["model"] = json!(model);
        }
        if let Some(effort) = self.resolved_effort(input.reasoning_effort) {
            params["effort"] = json!(effort.as_str());
        }
        // Codex spells fast mode as a service tier, and `priority` is the id
        // its own catalog labels "Fast". Sent only when armed: an explicit
        // `standard` on every turn would be the same noise the comment above
        // warns about, and off is already the thread's default.
        if input.fast_mode {
            params["serviceTier"] = json!(FAST_SERVICE_TIER);
        }
        let posture = self.pending_posture();
        if let Some(posture) = posture {
            let (sandbox, approval) = turn_start_policy(posture.mode);
            params["sandboxPolicy"] = sandbox;
            params["approvalPolicy"] = json!(approval);
        }
        let id = match self.request("turn/start", params).await {
            Ok(id) => id,
            Err(err) => {
                // The posture rode on the request that failed, so it is still
                // unsent. Clearing it here would leave the thread on its old
                // one with nothing left to correct it.
                self.abort_control_turn("the turn did not start");
                return Err(err);
            }
        };
        self.register_turn_admission(id, thread_id, posture.map(|generation| generation.id));
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

    /// Record the new posture; the next `turn/start` carries it.
    ///
    /// `turn/start`'s `approvalPolicy` and `sandboxPolicy` are documented as
    /// applying to this turn and subsequent turns, so this is the engine's own
    /// channel for re-posturing a thread — the child and its context stay.
    async fn set_permission_mode(&self, mode: PermissionMode) -> Result<(), HarnessError> {
        if self.permission_mode() == mode {
            return Ok(());
        }
        *self.permission_mode.lock().expect("codex permission mode") = mode;
        self.arm_posture(mode);
        Ok(())
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
        // Captured channel carries accept/decline only — no rejection string,
        // and none of the richer decision variants (caps say so; this is the
        // backstop).
        let token = match decision {
            ApprovalDecision::Approve => "accept",
            ApprovalDecision::Deny { .. } => "decline",
            ApprovalDecision::ApproveWithGrant { .. }
            | ApprovalDecision::Answers { .. }
            | ApprovalDecision::PlanDecision { .. } => {
                return Err(HarnessError::DecisionUnsupported(
                    "the codex approval channel takes accept or decline".into(),
                ));
            }
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
        if self.child_pid.load(Ordering::SeqCst) == 0 {
            // No child means nothing is running: a stop aimed at a parked
            // session must not cost it anything (decision 0064).
            return Ok(());
        }
        let asked = self.interrupts_this_turn.fetch_add(1, Ordering::SeqCst);
        if asked > 0 {
            return self.interrupt_process_tree().await;
        }
        let thread_id = self.resume_ref.lock().expect("codex resume").clone();
        let turn_id = self.active_control_turn_id();
        let (Some(thread_id), Some(turn_id)) = (thread_id, turn_id) else {
            return self.interrupt_process_tree().await;
        };

        let rpc_id = self.next_rpc_id();
        let receiver = self.register_interrupt(rpc_id);
        let message = json!({
            "id": rpc_id,
            "method": "turn/interrupt",
            "params": { "threadId": thread_id, "turnId": turn_id },
        });
        if let Err(err) = self.write_message(&message).await {
            self.cancel_interrupt(rpc_id, "the engine refused the interrupt request");
            warn!(%err, "engine child refused a stop request; stopping the process");
            return self.interrupt_process_tree().await;
        }

        match timeout(CONTROL_RPC_TIMEOUT, receiver).await {
            Ok(Ok(Ok(()))) => Ok(()),
            Ok(Ok(Err(err))) => {
                warn!(%err, "engine rejected a stop request; stopping the process");
                self.interrupt_process_tree().await
            }
            Ok(Err(_)) => self.interrupt_process_tree().await,
            Err(_) => {
                self.cancel_interrupt(
                    rpc_id,
                    "timed out waiting for the engine to acknowledge the interrupt",
                );
                self.interrupt_process_tree().await
            }
        }
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

    /// Terminate the idle server child (decision 0064). The thread stays
    /// resumable, and the next turn's ensure re-runs the handshake on a
    /// replacement.
    async fn park(&self) -> Result<(), HarnessError> {
        let mut slot = self.child.lock().await;
        *self.stdin.lock().expect("codex stdin") = None;
        *self.stdout.lock().expect("codex stdout") = None;
        self.child_pid.store(0, Ordering::SeqCst);
        if let Some(mut child) = slot.take() {
            let _ = child.terminate().await;
        }
        Ok(())
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
            None,
        )
        .unwrap_err();
        assert!(matches!(err, HarnessError::LaunchRejected(_)));
    }

    #[test]
    fn permission_mode_mapping_matches_0033() {
        assert_eq!(
            thread_start_policy(PermissionMode::Plan),
            ("read-only", "untrusted")
        );
        assert_eq!(
            thread_start_policy(PermissionMode::Ask),
            ("workspace-write", "untrusted")
        );
        assert_eq!(
            thread_start_policy(PermissionMode::Auto),
            ("workspace-write", "on-request")
        );
        assert_eq!(
            thread_start_policy(PermissionMode::Allow),
            ("danger-full-access", "never")
        );
        let _ = PathBuf::from("/workspace");
    }

    #[test]
    fn thread_loads_allow_a_longer_inactivity_window_than_initialization() {
        assert!(THREAD_LOAD_TIMEOUT > HANDSHAKE_TIMEOUT);
        assert_eq!(THREAD_LOAD_TIMEOUT, Duration::from_secs(120));
        assert!(THREAD_LOAD_ABSOLUTE_CEILING > THREAD_LOAD_TIMEOUT);
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

    /// The same stand-in, but its `thread/resume` succeeds — the engine still
    /// holds the thread, as it does after a park (decision 0064).
    #[cfg(unix)]
    const FAKE_RESUMABLE_APP_SERVER: &str = r#"#!/bin/sh
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
      printf '{"id":%s,"result":{"thread":{"id":"THREAD-1","cliVersion":"0.147.0","turns":[]}}}\n' "$id"
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
    const FAKE_POSTURE_APP_SERVER: &str = r#"#!/bin/sh
turns=0
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
      turns=$((turns+1))
      printf '%s\n' "$line" >>"$FAKE_CODEX_TURNS"
      if [ "$turns" -eq 1 ]; then
        printf '{"id":%s,"error":{"code":-32602,"message":"invalid turn options"}}\n' "$id"
      else
        printf '{"id":%s,"result":{"turn":{"id":"TURN-%s","status":"inProgress"}}}\n' "$id" "$turns"
        printf '{"method":"turn/completed","params":{"threadId":"THREAD-1","turn":{"id":"TURN-%s","status":"completed"}}}\n' "$turns"
      fi
      ;;
  esac
done
"#;

    #[cfg(unix)]
    const FAKE_INTERRUPT_APP_SERVER: &str = r#"#!/bin/sh
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
    *'"method":"turn/interrupt"'*)
      printf '%s\n' "$line" >>"$FAKE_CODEX_INTERRUPTS"
      case "$FAKE_CODEX_INTERRUPT_MODE" in
        success)
          printf '{"id":%s,"result":{}}\n' "$id"
          printf '{"method":"turn/completed","params":{"threadId":"THREAD-1","turn":{"id":"TURN-1","status":"interrupted"}}}\n'
          ;;
        error)
          printf '{"id":%s,"error":{"code":-32000,"message":"turn is no longer active"}}\n' "$id"
          ;;
        eof)
          exit 0
          ;;
        timeout)
          :
          ;;
      esac
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
            owner: tidebreak_core::OwnerId::local(),
            session_id: tidebreak_core::CodeSessionId::new(),
            worktree: PathBuf::from("."),
            allowed_read_roots: Vec::new(),
            permission_mode: PermissionMode::Auto,
            model: None,
            reasoning_effort: None,
            fast_mode: false,
            resume_ref: Some("THREAD-1".into()),
            extra_argv: Vec::new(),
            extra_env: Vec::new(),
            relay_key_env: None,
            env: Vec::new(),
            approval: None,
            binary: Some(PathBuf::from("codex")),
            sink,
            browser: None,
        })
    }

    #[cfg(unix)]
    async fn session_reading_script(script: &str) -> (CodexSession, tokio::process::Child) {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(script)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let session = unit_session(Arc::new(SilentSink));
        *session.stdout.lock().expect("codex stdout") =
            Some(Arc::new(AsyncMutex::new(StdoutReader {
                stdout,
                lines: StreamLineBuffer::new(),
            })));
        (session, child)
    }

    /// History that keeps arriving past one inactivity window must still
    /// complete: the old fixed deadline would kill this restore.
    #[cfg(unix)]
    #[tokio::test]
    async fn read_until_rpc_resets_inactivity_timeout_on_each_batch() {
        let inactivity = Duration::from_millis(200);
        let (session, mut child) = session_reading_script(
            r#"
i=0
while [ "$i" -lt 4 ]; do
  printf '{"method":"item/completed","params":{"id":%s}}\n' "$i"
  sleep 0.12
  i=$((i + 1))
done
printf '{"id":7,"result":{"thread":{"id":"THREAD-1"}}}\n'
sleep 2
"#,
        )
        .await;
        session
            .read_until_rpc(7, inactivity)
            .await
            .expect("streaming restore should outlive one inactivity window");
        let _ = child.kill().await;
    }

    /// A child that goes silent is still bounded by one inactivity window.
    #[cfg(unix)]
    #[tokio::test]
    async fn read_until_rpc_times_out_when_the_child_goes_silent() {
        let inactivity = Duration::from_millis(200);
        let (session, mut child) = session_reading_script("sleep 2").await;
        let err = session.read_until_rpc(7, inactivity).await.unwrap_err();
        match err {
            HarnessError::Other(message) => {
                assert!(
                    message.contains("timed out waiting for rpc id 7"),
                    "unexpected timeout message: {message}"
                );
            }
            other => panic!("expected inactivity timeout, got {other:?}"),
        }
        let _ = child.kill().await;
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
            owner: tidebreak_core::OwnerId::local(),
            session_id: tidebreak_core::CodeSessionId::new(),
            worktree: dir.to_path_buf(),
            allowed_read_roots: Vec::new(),
            permission_mode: PermissionMode::Auto,
            model: None,
            reasoning_effort: None,
            fast_mode: false,
            resume_ref,
            extra_argv: Vec::new(),
            extra_env: vec![(
                "FAKE_CODEX_CALLS".into(),
                dir.join("calls").to_string_lossy().into_owned(),
            )],
            relay_key_env: None,
            env: Vec::new(),
            approval: None,
            binary: Some(binary.to_path_buf()),
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

    #[cfg(unix)]
    fn turn(text: &str) -> TurnInput {
        TurnInput {
            text: text.into(),
            model: None,
            reasoning_effort: None,
            fast_mode: false,
            images: Vec::new(),
        }
    }

    #[cfg(unix)]
    async fn run_interrupt_case(
        mode: &str,
    ) -> (
        Result<TurnOutcome, HarnessError>,
        Result<(), HarnessError>,
        bool,
        bool,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("codex");
        write_app_server(&binary, FAKE_INTERRUPT_APP_SERVER);
        let mut spec = spec_for(dir.path(), &binary, None);
        spec.extra_env
            .push(("FAKE_CODEX_INTERRUPT_MODE".into(), mode.to_owned()));
        spec.extra_env.push((
            "FAKE_CODEX_INTERRUPTS".into(),
            dir.path()
                .join("interrupts.ndjson")
                .to_string_lossy()
                .into_owned(),
        ));
        let session = Arc::new(CodexSession::new(spec));
        let running = tokio::spawn({
            let session = Arc::clone(&session);
            async move { session.run_turn(turn("keep working")).await }
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            while session.active_control_turn_id().is_none() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("fake turn was never admitted");

        let stopped = session.interrupt().await;
        let outcome = tokio::time::timeout(Duration::from_secs(2), running)
            .await
            .expect("fake turn did not finish")
            .expect("turn task panicked");
        let child_alive = session.child_pid().is_some();
        let request_written = std::fs::read_to_string(dir.path().join("interrupts.ndjson"))
            .is_ok_and(|input| input.lines().count() == 1);
        session.park().await.unwrap();
        (outcome, stopped, child_alive, request_written)
    }

    /// The wedge from the app-server dying before its first turn: codex
    /// never persisted the thread, so a thread id that has run no turn must
    /// not be reported as a resume ref. The next spawn then starts a clean
    /// thread.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_thread_that_ran_no_turn_is_not_a_resume_ref() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("codex");
        write_fake_app_server(&binary);

        let session = CodexSession::new(spec_for(dir.path(), &binary, None));
        assert!(
            calls(dir.path()).is_empty(),
            "nothing spawns before the first turn (decision 0064)"
        );
        assert_eq!(session.child_pid(), None);
        assert_eq!(
            session.resume_ref(),
            None,
            "a thread with no turns is not resumable and must not be persisted"
        );

        session.run_turn(turn("first turn")).await.unwrap();
        assert_eq!(
            calls(dir.path()),
            ["initialize", "thread/start", "turn/start"],
            "the first turn spawns, handshakes, and runs"
        );
        assert_eq!(session.resume_ref().as_deref(), Some("THREAD-1"));
    }

    /// A resume ref the engine no longer knows is a lost resume, not a turn
    /// failure: the server fences on this rather than failing every turn.
    /// With the child spawned on the first turn (decision 0064), that is
    /// where the stored ref meets the engine.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_stale_resume_ref_reports_a_lost_resume() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("codex");
        write_fake_app_server(&binary);

        let session = CodexSession::new(spec_for(
            dir.path(),
            &binary,
            Some("STALE-THREAD".to_owned()),
        ));
        let Err(err) = session.run_turn(turn("first turn")).await else {
            panic!("a turn on an unknown thread must not succeed");
        };
        assert_eq!(calls(dir.path()), ["initialize", "thread/resume"]);
        let HarnessError::ResumeLost(detail) = err else {
            panic!("expected a lost resume, got {err}");
        };
        assert!(detail.contains("thread not found"), "detail: {detail}");
        assert_eq!(
            session.child_pid(),
            None,
            "a failed handshake leaves no half-attached child behind"
        );
    }

    /// Decision 0064: a parked thread that has run resumes on a replacement
    /// child, with `thread/resume` on the wire and the same thread id kept.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_parked_thread_is_resumed_on_the_next_turn() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("codex");
        write_app_server(&binary, FAKE_RESUMABLE_APP_SERVER);

        let session = CodexSession::new(spec_for(dir.path(), &binary, None));
        session.run_turn(turn("one")).await.unwrap();
        let first_pid = session.child_pid().expect("the child outlives its turn");

        session.park().await.unwrap();
        assert_eq!(session.child_pid(), None, "the parked child is gone");

        session.run_turn(turn("two")).await.unwrap();
        assert_eq!(
            calls(dir.path()),
            [
                "initialize",
                "thread/start",
                "turn/start",
                "initialize",
                "thread/resume",
                "turn/start"
            ],
            "the wake respawns and resumes rather than starting a new thread"
        );
        let second_pid = session.child_pid().expect("the wake turn spawned a child");
        assert_ne!(first_pid, second_pid, "a new process answered the wake");
        assert_eq!(session.resume_ref().as_deref(), Some("THREAD-1"));
    }

    /// Codex never persisted a thread that ran no turn, so waking one must
    /// start clean. Resuming it would fence the session on "thread not
    /// found" — the fake's own answer catches a wrong ensure here.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_parked_thread_that_never_ran_is_restarted_not_resumed() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("codex");
        write_fake_app_server(&binary);

        let session = CodexSession::new(spec_for(dir.path(), &binary, None));
        session.ensure_child().await.unwrap();
        session.park().await.unwrap();
        session.ensure_child().await.unwrap();
        assert_eq!(
            calls(dir.path()),
            ["initialize", "thread/start", "initialize", "thread/start"],
            "an unwritten thread is restarted, never resumed"
        );
    }

    /// A stop aimed at a parked session must not fail and must not spawn
    /// anything (decision 0064).
    #[tokio::test]
    async fn an_interrupt_with_no_child_is_a_no_op() {
        let session = unit_session(Arc::new(RecordingSink::default()));
        session.interrupt().await.unwrap();
        assert_eq!(session.child_pid(), None);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rejected_turn_start_keeps_posture_pending() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("codex");
        write_app_server(&binary, FAKE_POSTURE_APP_SERVER);
        let mut spec = spec_for(dir.path(), &binary, None);
        spec.extra_env.push((
            "FAKE_CODEX_TURNS".into(),
            dir.path()
                .join("turns.ndjson")
                .to_string_lossy()
                .into_owned(),
        ));
        let session = CodexSession::new(spec);
        session
            .set_permission_mode(PermissionMode::Plan)
            .await
            .unwrap();

        session.run_turn(turn("first")).await.unwrap();
        assert!(
            session.pending_posture().is_some(),
            "a rejected turn must leave its posture armed"
        );
        session.run_turn(turn("second")).await.unwrap();
        assert!(
            session.pending_posture().is_none(),
            "the matching accepted retry settles the posture"
        );

        let requests = std::fs::read_to_string(dir.path().join("turns.ndjson")).unwrap();
        let requests = requests
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(requests.len(), 2);
        for request in requests {
            assert_eq!(request["params"]["sandboxPolicy"]["type"], "readOnly");
            assert_eq!(request["params"]["approvalPolicy"], "untrusted");
        }
    }

    #[tokio::test]
    async fn late_success_cannot_clear_newer_posture_generation() {
        let session = unit_session(Arc::new(RecordingSink::default()));
        let first = session.pending_posture().expect("resume posture is armed");
        session.register_turn_admission(41, "THREAD-1".into(), Some(first.id));
        let newer = session.arm_posture(PermissionMode::Plan);

        session
            .emit_parsed(r#"{"id":41,"result":{"turn":{"id":"TURN-OLD"}}}"#)
            .await;

        assert_eq!(session.pending_posture(), Some(newer));
    }

    #[test]
    fn admission_timeout_keeps_posture_pending() {
        let session = unit_session(Arc::new(RecordingSink::default()));
        let generation = session.pending_posture().expect("resume posture is armed");
        session.register_turn_admission(41, "THREAD-1".into(), Some(generation.id));
        session
            .posture
            .lock()
            .expect("codex posture")
            .admission
            .as_mut()
            .expect("turn admission")
            .deadline = Instant::now();

        assert!(session.expire_turn_admission());
        assert_eq!(session.pending_posture(), Some(generation));
    }

    #[tokio::test]
    async fn malformed_turn_start_result_keeps_posture_pending() {
        let session = unit_session(Arc::new(RecordingSink::default()));
        let generation = session.pending_posture().expect("resume posture is armed");
        session
            .parser
            .lock()
            .expect("codex parser")
            .note_outbound(&json!(41), "turn/start");
        session.register_turn_admission(41, "THREAD-1".into(), Some(generation.id));

        let events = session.emit_parsed(r#"{"id":41,"result":{}}"#).await;

        assert!(matches!(
            events.last(),
            Some(HarnessEvent::TurnFailed { .. })
        ));
        assert_eq!(session.pending_posture(), Some(generation));
    }

    #[tokio::test]
    async fn terminal_before_admission_keeps_posture_pending() {
        let session = unit_session(Arc::new(RecordingSink::default()));
        let generation = session.pending_posture().expect("resume posture is armed");
        session.register_turn_admission(41, "THREAD-1".into(), Some(generation.id));

        session
            .emit_parsed(
                r#"{"method":"turn/completed","params":{"threadId":"THREAD-1","turn":{"id":"TURN-1","status":"failed"}}}"#,
            )
            .await;

        assert_eq!(session.pending_posture(), Some(generation));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn codex_interrupt_waits_for_correlated_success() {
        let (outcome, stopped, child_alive, request_written) = run_interrupt_case("success").await;
        stopped.unwrap();
        assert!(matches!(outcome.unwrap(), TurnOutcome::Clean));
        assert!(child_alive, "a native interrupt keeps the session child");
        assert!(request_written);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn codex_interrupt_error_falls_back_to_the_process_tree() {
        let (outcome, stopped, child_alive, request_written) = run_interrupt_case("error").await;
        stopped.unwrap();
        assert!(outcome.is_err());
        assert!(!child_alive);
        assert!(request_written);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn codex_interrupt_timeout_falls_back_to_the_process_tree() {
        let (outcome, stopped, child_alive, request_written) = run_interrupt_case("timeout").await;
        stopped.unwrap();
        assert!(outcome.is_err());
        assert!(!child_alive);
        assert!(request_written);
    }

    #[tokio::test]
    async fn a_late_codex_interrupt_response_cannot_resolve_a_new_waiter() {
        let session = unit_session(Arc::new(RecordingSink::default()));
        let old = session.register_interrupt(52);
        session.cancel_interrupt(52, "timed out");
        assert!(old.await.unwrap().is_err());
        let current = session.register_interrupt(53);

        session.emit_parsed(r#"{"id":52,"result":{}}"#).await;
        assert_eq!(
            session
                .control_state
                .lock()
                .expect("codex control state")
                .interrupt
                .as_ref()
                .map(|pending| pending.rpc_id),
            Some(53)
        );

        session.emit_parsed(r#"{"id":53,"result":{}}"#).await;
        current.await.unwrap().unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn codex_interrupt_eof_falls_back_to_the_process_tree() {
        let (outcome, stopped, child_alive, request_written) = run_interrupt_case("eof").await;
        stopped.unwrap();
        assert!(outcome.is_err());
        assert!(!child_alive);
        assert!(request_written);
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
        let session = Arc::new(CodexSession::new(spec));
        let running = tokio::spawn({
            let session = Arc::clone(&session);
            async move {
                session
                    .run_turn(TurnInput {
                        text: "first turn".into(),
                        model: None,
                        reasoning_effort: None,
                        fast_mode: false,
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
            None,
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
            None,
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
            None,
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
            None,
        )
        .unwrap();
        let has_reserved = plan
            .env
            .iter()
            .any(|(key, _)| BrowserChannelSpec::is_reserved_env_key(key));
        assert!(!has_reserved);
    }

    #[test]
    fn session_relay_key_survives_the_reserved_namespace_strip() {
        // Decision 71 hands the codex child its per-session relay key by env
        // (the provider config spawn_wiring emits reads it through
        // env_key={relay_key_env}); stripping it as a reserved key left
        // hosted codex sessions with no credential at all. Only the exact
        // wired name survives; the rest of the namespace stays reserved.
        let plan = compose_app_server_plan(
            std::path::Path::new("/usr/bin/codex"),
            &[],
            std::path::Path::new("/workspace"),
            &[
                ("TIDEBREAK_LLM_KEY".into(), "tbreak_hl_test".into()),
                ("TIDEBREAK_BROWSER_CAPFILE".into(), "/evil/cap.json".into()),
            ],
            None,
            Some("TIDEBREAK_LLM_KEY"),
        )
        .unwrap();
        let relay = plan.env.iter().find(|(key, _)| key == "TIDEBREAK_LLM_KEY");
        assert_eq!(
            relay.map(|(_, value)| value.as_str()),
            Some("tbreak_hl_test")
        );
        assert!(!plan
            .env
            .iter()
            .any(|(key, _)| key == "TIDEBREAK_BROWSER_CAPFILE"));
    }

    #[test]
    fn relay_key_is_stripped_when_no_relay_is_wired() {
        // Without a wired relay there is no exception: a settings value
        // squatting on a relay-shaped name is Tidebreak's namespace, not
        // the user's.
        let plan = compose_app_server_plan(
            std::path::Path::new("/usr/bin/codex"),
            &[],
            std::path::Path::new("/workspace"),
            &[("TIDEBREAK_LLM_KEY".into(), "from-settings".into())],
            None,
            None,
        )
        .unwrap();
        assert!(plan.env.is_empty(), "{:?}", plan.env);
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
            None,
        )
        .unwrap();
        let override_idx = plan.argv.iter().position(|arg| arg == "-c").unwrap();
        let value = &plan.argv[override_idx + 1];
        // The command must appear as one quoted value, not split on spaces.
        assert!(value.contains("command=\"/Applications/Tidebreak.app/Contents/bin/tidebreak\""));
        // args must still be exactly ["browser-mcp"].
        assert!(value.contains(r#"args=["browser-mcp"]"#));
    }

    #[test]
    fn bridge_command_with_backslashes_is_escaped() {
        // A Windows path like C:\bin\tidebreak.exe must have its backslashes
        // JSON-escaped so the config string remains valid.
        let spec = BrowserChannelSpec::new(
            std::path::PathBuf::from("/tmp/browser-cap.json"),
            std::path::PathBuf::from(r"C:\Program Files\Tidebreak\tidebreak.exe"),
        );
        let plan = compose_app_server_plan(
            std::path::Path::new("/usr/bin/codex"),
            &[],
            std::path::Path::new("/workspace"),
            &[],
            Some(&spec),
            None,
        )
        .unwrap();
        let override_idx = plan.argv.iter().position(|arg| arg == "-c").unwrap();
        let value = &plan.argv[override_idx + 1];
        // The backslashes must be escaped as \\, not left raw.
        assert!(value.contains("\\\\"));
        // The original \t in Tidebreak must not become a tab character.
        assert!(!value.contains('\t'));
        // The command must still parse as one value.
        assert!(value.contains("command=\""));
        assert!(value.contains(r#"args=["browser-mcp"]"#));
    }

    #[test]
    fn bridge_command_with_embedded_quote_is_escaped() {
        // A path containing a double-quote (unlikely but defensive) must
        // escape it with a backslash so the config value remains valid.
        let spec = BrowserChannelSpec::new(
            std::path::PathBuf::from("/tmp/browser-cap.json"),
            std::path::PathBuf::from("/tmp/tidebreak-\"-binary"),
        );
        let plan = compose_app_server_plan(
            std::path::Path::new("/usr/bin/codex"),
            &[],
            std::path::Path::new("/workspace"),
            &[],
            Some(&spec),
            None,
        )
        .unwrap();
        let override_idx = plan.argv.iter().position(|arg| arg == "-c").unwrap();
        let value = &plan.argv[override_idx + 1];
        // The embedded " must be escaped as \", not raw.
        assert!(value.contains("\\\""));
        // The surrounding command="..." delimiter must still close properly.
        assert!(value.starts_with("mcp_servers.tb-browser="));
        assert!(value.contains("command=\""));
        assert!(value.ends_with("]}"));
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_bridge_command_is_rejected_instead_of_changed() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let spec = BrowserChannelSpec::new(
            std::path::PathBuf::from("/tmp/browser-cap.json"),
            std::path::PathBuf::from(OsString::from_vec(b"/tmp/tidebreak-\xff".to_vec())),
        );
        let error = compose_app_server_plan(
            std::path::Path::new("/usr/bin/codex"),
            &[],
            std::path::Path::new("/workspace"),
            &[],
            Some(&spec),
            None,
        )
        .expect_err("non-UTF-8 bridge paths must fail closed");

        assert!(error.to_string().contains("not valid UTF-8"));
    }
}
