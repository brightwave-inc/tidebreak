//! A [`HarnessAdapter`] / [`HarnessSession`] driven by a script of events.
//!
//! Compiled only under the `scripted-harness` feature or in this crate's
//! tests, matching [`scripted_provider`]: a released binary never contains it.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tidebreak_core::{CapLevel, HarnessCaps, HarnessKind, PermissionMode, ReasoningEffort};
use tidebreak_harness::child::ChildPid;
use tidebreak_harness::{
    ApprovalCompleter, ApprovalDecision, HarnessAdapter, HarnessApprovalRef, HarnessError,
    HarnessEvent, HarnessEventSink, HarnessProbe, HarnessSession, HostEnv, SessionSpec, TurnInput,
    TurnOutcome,
};
use tokio::sync::{oneshot, watch};

/// Completer that records decisions and unparks a scripted turn.
///
/// A decision can arrive before [`Self::park`] if the worker multiplexes it
/// in the gap after `ApprovalRequested` is journaled. Stash it in that case.
#[derive(Default)]
pub(crate) struct ScriptedApprover {
    parked: std::sync::Mutex<Option<oneshot::Sender<ApprovalDecision>>>,
    staged: std::sync::Mutex<Option<ApprovalDecision>>,
    observed: std::sync::Mutex<Vec<(String, ApprovalDecision)>>,
}

impl ScriptedApprover {
    pub(crate) fn park(&self) -> oneshot::Receiver<ApprovalDecision> {
        let (tx, rx) = oneshot::channel();
        if let Some(decision) = self.staged.lock().expect("scripted staged").take() {
            let _ = tx.send(decision);
            return rx;
        }
        *self.parked.lock().expect("scripted park") = Some(tx);
        rx
    }

    pub(crate) fn observed(&self) -> Vec<(String, ApprovalDecision)> {
        self.observed.lock().expect("scripted observed").clone()
    }
}

#[async_trait]
impl ApprovalCompleter for ScriptedApprover {
    async fn complete(
        &self,
        call_id: &str,
        decision: ApprovalDecision,
    ) -> Result<(), HarnessError> {
        self.observed
            .lock()
            .expect("scripted observed")
            .push((call_id.to_owned(), decision.clone()));
        if let Some(tx) = self.parked.lock().expect("scripted park").take() {
            let _ = tx.send(decision);
        } else {
            *self.staged.lock().expect("scripted staged") = Some(decision);
        }
        Ok(())
    }
}

/// Environment variable carrying a JSON array of [`HarnessEvent`]s.
#[allow(dead_code)]
const SCRIPT_VAR: &str = "TIDEBREAK_SCRIPTED_HARNESS";

/// One scripted engine session.
#[derive(Clone)]
pub(crate) struct ScriptedAdapter {
    kind: HarnessKind,
    events: Vec<HarnessEvent>,
    delay: Duration,
    mid_turn_steering: CapLevel,
    steering_delay: Duration,
    steering_rejection: Option<String>,
    structured_approvals: CapLevel,
    auto_mode: CapLevel,
    allow_mode: CapLevel,
    image_input: CapLevel,
    reasoning_levels: CapLevel,
    /// Whether this engine takes a new mode without being relaunched.
    live_mode_switch: bool,
    /// Whether this engine fixes its posture when it creates its session and
    /// keeps it across a resume, the way opencode does.
    posture_fixed: bool,
    /// The effort each turn ran at, and every mode a live switch moved this
    /// engine onto. Shared with the session so both survive a relaunch.
    turns: Arc<std::sync::Mutex<Vec<Option<ReasoningEffort>>>>,
    modes: Arc<std::sync::Mutex<Vec<PermissionMode>>>,
    /// What each turn actually handed the engine, for a test that cares how
    /// an attachment travelled rather than that it was accepted.
    inputs: Arc<std::sync::Mutex<Vec<ScriptedTurnInput>>>,
    child_pid: Option<i64>,
    unrecognized_per_turn: u64,
    silent_interrupt: bool,
    lost_resume: Option<String>,
    park_approvals: bool,
    approver: Arc<ScriptedApprover>,
    probes: Arc<AtomicU64>,
    /// Approval endpoint each launch was handed, `None` when it was handed no
    /// channel at all. Lets a test see how a session was wired.
    launched_approvals: Arc<std::sync::Mutex<Vec<Option<String>>>>,
}

impl ScriptedAdapter {
    pub(crate) fn new(events: Vec<HarnessEvent>) -> Self {
        Self {
            kind: HarnessKind::ClaudeCode,
            events,
            delay: Duration::ZERO,
            mid_turn_steering: CapLevel::Unsupported,
            steering_delay: Duration::ZERO,
            steering_rejection: None,
            structured_approvals: CapLevel::Unsupported,
            auto_mode: CapLevel::Unsupported,
            allow_mode: CapLevel::Unsupported,
            image_input: CapLevel::Unknown,
            reasoning_levels: CapLevel::Unsupported,
            live_mode_switch: false,
            posture_fixed: false,
            turns: Arc::new(std::sync::Mutex::new(Vec::new())),
            modes: Arc::new(std::sync::Mutex::new(Vec::new())),
            inputs: Arc::new(std::sync::Mutex::new(Vec::new())),
            child_pid: None,
            unrecognized_per_turn: 0,
            silent_interrupt: false,
            lost_resume: None,
            park_approvals: true,
            approver: Arc::new(ScriptedApprover::default()),
            probes: Arc::new(AtomicU64::new(0)),
            launched_approvals: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    /// How many times this adapter has been probed. The runtime memoizes
    /// probes, so this is how a test sees a cache hit from a cold read.
    #[allow(dead_code)]
    pub(crate) fn probe_count(&self) -> u64 {
        self.probes.load(Ordering::SeqCst)
    }

    /// The approval endpoint each launched session was given, in order.
    #[allow(dead_code)]
    pub(crate) fn launched_approvals(&self) -> Vec<Option<String>> {
        self.launched_approvals
            .lock()
            .expect("scripted launches")
            .clone()
    }

    /// Fails every turn the way an engine does once it has lost the session
    /// this one resumed: not a turn failure, a dead resume ref.
    pub(crate) fn with_lost_resume(mut self, detail: &str) -> Self {
        self.lost_resume = Some(detail.to_owned());
        self
    }

    /// Sets the approval channel and, with it, the supervised auto posture —
    /// the coupling every real approval-carrying adapter has.
    pub(crate) fn with_approvals(mut self, level: CapLevel) -> Self {
        self.structured_approvals = level;
        self.auto_mode = level;
        self
    }

    /// Overrides the auto posture independently of the approval channel,
    /// for exercising the mode gate's per-flag refusals.
    pub(crate) fn with_auto_mode(mut self, level: CapLevel) -> Self {
        self.auto_mode = level;
        self
    }

    /// Overrides the allow-everything posture independently of Auto.
    pub(crate) fn with_allow_mode(mut self, level: CapLevel) -> Self {
        self.allow_mode = level;
        self
    }

    /// Declares whether this scripted engine consumes image attachments.
    pub(crate) fn with_image_input(mut self, level: CapLevel) -> Self {
        self.image_input = level;
        self
    }

    /// Declares an effort ladder, the way every adapter but opencode has one.
    pub(crate) fn with_reasoning_levels(mut self, level: CapLevel) -> Self {
        self.reasoning_levels = level;
        self
    }

    /// Model an engine that re-postures a live session on its own channel,
    /// the way Claude Code and Codex do. Left off, the runtime relaunches.
    pub(crate) fn with_live_mode_switch(mut self) -> Self {
        self.live_mode_switch = true;
        self
    }

    /// Model opencode: the posture rides session creation, the session reports
    /// a resume ref, and resuming it does not re-apply the posture — so a
    /// relaunch would come back running the old mode.
    pub(crate) fn with_posture_fixed_at_session_start(mut self) -> Self {
        self.posture_fixed = true;
        self
    }

    /// What each turn handed the engine, in order.
    pub(crate) fn turn_inputs(&self) -> Vec<ScriptedTurnInput> {
        self.inputs.lock().expect("scripted inputs").clone()
    }

    /// The effort each turn actually ran at, in order.
    pub(crate) fn turn_efforts(&self) -> Vec<Option<ReasoningEffort>> {
        self.turns.lock().expect("scripted turns").clone()
    }

    /// Every mode a live switch moved this engine onto, in order.
    pub(crate) fn live_modes(&self) -> Vec<PermissionMode> {
        self.modes.lock().expect("scripted modes").clone()
    }

    #[allow(dead_code)]
    pub(crate) fn observed_decisions(&self) -> Vec<(String, ApprovalDecision)> {
        self.approver.observed()
    }

    pub(crate) fn with_delay(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }

    pub(crate) fn with_steering(mut self, level: CapLevel) -> Self {
        self.mid_turn_steering = level;
        self
    }

    pub(crate) fn with_steering_delay(mut self, delay: Duration) -> Self {
        self.mid_turn_steering = CapLevel::Supported;
        self.steering_delay = delay;
        self
    }

    pub(crate) fn with_steering_rejection(mut self, detail: impl Into<String>) -> Self {
        self.mid_turn_steering = CapLevel::Supported;
        self.steering_rejection = Some(detail.into());
        self
    }

    /// Publish this pid for the duration of each turn, the way an adapter
    /// that spawns one child per turn does.
    #[allow(dead_code)]
    pub(crate) fn with_child_pid(mut self, pid: i64) -> Self {
        self.child_pid = Some(pid);
        self
    }

    /// Model an engine that asks for an approval and then stops waiting for
    /// one, the way Claude Code's own 60-second permission-prompt timeout
    /// does: the script plays straight past the request.
    pub(crate) fn with_unattended_approvals(mut self) -> Self {
        self.park_approvals = false;
        self
    }

    /// Stands in for a parser that could not map part of the stream: the
    /// scripted session reports this many unrecognized events per turn.
    pub(crate) fn with_unrecognized_per_turn(mut self, count: u64) -> Self {
        self.unrecognized_per_turn = count;
        self
    }

    /// Model an engine that dies without saying anything — a SIGKILLed child
    /// reaches EOF exactly like a finished one.
    #[allow(dead_code)]
    pub(crate) fn with_silent_interrupt(mut self) -> Self {
        self.silent_interrupt = true;
        self
    }
}

#[async_trait]
impl HarnessAdapter for ScriptedAdapter {
    fn kind(&self) -> HarnessKind {
        self.kind
    }

    async fn probe(&self, _host: &HostEnv) -> HarnessProbe {
        self.probes.fetch_add(1, Ordering::SeqCst);
        HarnessProbe {
            found: true,
            binary_path: Some(PathBuf::from("/scripted/engine")),
            version: Some("scripted".into()),
            authenticated: Some(true),
            stderr: String::new(),
            env: Vec::new(),
            commands: Vec::new(),
        }
    }

    fn capabilities(&self, _probe: &HarnessProbe) -> HarnessCaps {
        HarnessCaps {
            resume: CapLevel::Supported,
            streaming_deltas: CapLevel::Supported,
            structured_approvals: self.structured_approvals,
            mid_turn_steering: self.mid_turn_steering,
            plan_mode: CapLevel::Supported,
            auto_mode: self.auto_mode,
            allow_mode: self.allow_mode,
            reasoning_levels: self.reasoning_levels,
            native_file_change_events: CapLevel::Unsupported,
            native_interrupt: CapLevel::Supported,
            image_input: self.image_input,
            slash_commands: CapLevel::Unknown,
        }
    }

    fn relaunch_composes_permission_mode(&self) -> bool {
        !self.posture_fixed
    }

    async fn launch(&self, spec: SessionSpec) -> Result<Box<dyn HarnessSession>, HarnessError> {
        self.launched_approvals
            .lock()
            .expect("scripted launches")
            .push(
                spec.approval
                    .as_ref()
                    .map(|channel| channel.mcp_endpoint_url.clone()),
            );
        Ok(Box::new(ScriptedSession {
            sink: spec.sink,
            events: self.events.clone(),
            delay: self.delay,
            mid_turn_steering: self.mid_turn_steering,
            steering_delay: self.steering_delay,
            steering_rejection: self.steering_rejection.clone(),
            child_pid: self.child_pid,
            silent_interrupt: self.silent_interrupt,
            pid: ChildPid::new(),
            lost_resume: self.lost_resume.clone(),
            park_approvals: self.park_approvals,
            interrupt: Arc::new(AtomicBool::new(false)),
            unrecognized: AtomicU64::new(0),
            unrecognized_per_turn: self.unrecognized_per_turn,
            approver: self.approver.clone(),
            live_mode_switch: self.live_mode_switch,
            posture_fixed: self.posture_fixed,
            turns: self.turns.clone(),
            modes: self.modes.clone(),
            inputs: self.inputs.clone(),
        }))
    }
}

struct ScriptedSession {
    sink: Arc<dyn HarnessEventSink>,
    events: Vec<HarnessEvent>,
    delay: Duration,
    mid_turn_steering: CapLevel,
    steering_delay: Duration,
    steering_rejection: Option<String>,
    child_pid: Option<i64>,
    silent_interrupt: bool,
    pid: ChildPid,
    lost_resume: Option<String>,
    park_approvals: bool,
    interrupt: Arc<AtomicBool>,
    unrecognized: AtomicU64,
    unrecognized_per_turn: u64,
    approver: Arc<ScriptedApprover>,
    live_mode_switch: bool,
    posture_fixed: bool,
    /// Shared with the adapter so a test can read what the engine was told
    /// after the runtime has dropped and relaunched the session.
    turns: Arc<std::sync::Mutex<Vec<Option<ReasoningEffort>>>>,
    modes: Arc<std::sync::Mutex<Vec<PermissionMode>>>,
    inputs: Arc<std::sync::Mutex<Vec<ScriptedTurnInput>>>,
}

/// One turn as the engine received it.
#[derive(Clone, Debug)]
pub(crate) struct ScriptedTurnInput {
    /// The prompt text, which is not always the message the person typed.
    pub text: String,
    /// How many images rode the protocol.
    pub images: usize,
}

impl ScriptedSession {
    /// Emit the script, stopping early when the worker interrupts.
    async fn play_script(&self) -> TurnOutcome {
        for event in &self.events {
            if self.interrupt.load(Ordering::SeqCst) {
                return self.stopped().await;
            }
            if !self.delay.is_zero() {
                tokio::time::sleep(self.delay).await;
            }
            if let HarnessEvent::ApprovalRequested { harness_ref, .. } = event {
                self.sink.emit(event.clone()).await;
                if !self.park_approvals {
                    continue;
                }
                let rx = self.approver.park();
                if let Ok(decision) = rx.await {
                    self.sink
                        .emit(HarnessEvent::ApprovalResolved {
                            harness_ref: harness_ref.clone(),
                            decision,
                        })
                        .await;
                }
                continue;
            }
            self.sink.emit(event.clone()).await;
            if matches!(
                event,
                HarnessEvent::TurnCompleted { .. }
                    | HarnessEvent::TurnFailed { .. }
                    | HarnessEvent::TurnInterrupted
            ) {
                return TurnOutcome::Clean;
            }
        }
        if self.interrupt.load(Ordering::SeqCst) {
            return self.stopped().await;
        }
        TurnOutcome::Clean
    }

    /// How a stopped engine ends: either it reports the abort on the stream,
    /// or — `silent_interrupt` — it dies with nothing to say, which is what a
    /// SIGKILLed child does.
    async fn stopped(&self) -> TurnOutcome {
        if self.silent_interrupt {
            return TurnOutcome::Incomplete {
                detail: "the engine was terminated by signal 9".into(),
            };
        }
        self.sink.emit(HarnessEvent::TurnInterrupted).await;
        TurnOutcome::Clean
    }
}

#[async_trait]
impl HarnessSession for ScriptedSession {
    async fn run_turn(&self, _input: TurnInput) -> Result<TurnOutcome, HarnessError> {
        self.turns
            .lock()
            .expect("scripted turns")
            .push(_input.reasoning_effort);
        self.inputs
            .lock()
            .expect("scripted inputs")
            .push(ScriptedTurnInput {
                text: _input.text.clone(),
                images: _input.images.len(),
            });
        if let Some(detail) = &self.lost_resume {
            return Err(HarnessError::ResumeLost(detail.clone()));
        }
        self.interrupt.store(false, Ordering::SeqCst);
        self.unrecognized
            .fetch_add(self.unrecognized_per_turn, Ordering::SeqCst);
        // A per-turn child exists only while the turn runs; publish it the way
        // a real one does so the worker records the pid mid-turn.
        self.pid
            .set(self.child_pid.map(|pid| pid as u32).filter(|pid| *pid != 0));
        let outcome = self.play_script().await;
        self.pid.clear();
        Ok(outcome)
    }

    async fn decide(
        &self,
        approval: HarnessApprovalRef,
        decision: ApprovalDecision,
    ) -> Result<(), HarnessError> {
        self.approver.complete(&approval.call_id, decision).await
    }

    async fn set_permission_mode(&self, mode: PermissionMode) -> Result<(), HarnessError> {
        if !self.live_mode_switch {
            return Err(HarnessError::PermissionModeSwitchUnsupported);
        }
        self.modes.lock().expect("scripted modes").push(mode);
        Ok(())
    }

    async fn interrupt(&self) -> Result<(), HarnessError> {
        self.interrupt.store(true, Ordering::SeqCst);
        // Drop a parked approval wait so run_turn can observe the flag.
        let _ = self.approver.parked.lock().expect("scripted park").take();
        let _ = self.approver.staged.lock().expect("scripted staged").take();
        Ok(())
    }

    async fn steer(&self, text: String) -> Result<(), HarnessError> {
        if self.mid_turn_steering != CapLevel::Supported {
            return Err(HarnessError::SteeringUnsupported);
        }
        if !self.steering_delay.is_zero() {
            tokio::time::sleep(self.steering_delay).await;
        }
        if let Some(detail) = &self.steering_rejection {
            return Err(HarnessError::SteeringRejected(detail.clone()));
        }
        self.sink.emit(HarnessEvent::UserSteered { text }).await;
        Ok(())
    }

    fn resume_ref(&self) -> Option<String> {
        // Only the posture-fixed engine reports one: it is what makes a
        // relaunch resume rather than create, and therefore what makes the
        // relaunch silent about a mode change.
        self.posture_fixed
            .then(|| "scripted-fixed-posture".to_owned())
    }

    fn child_pid(&self) -> Option<i64> {
        self.pid.get()
    }

    fn child_pid_changes(&self) -> Option<watch::Receiver<Option<i64>>> {
        Some(self.pid.subscribe())
    }

    fn unrecognized_events(&self) -> u64 {
        self.unrecognized.load(Ordering::SeqCst)
    }

    async fn shutdown(self: Box<Self>) -> Result<(), HarnessError> {
        Ok(())
    }
}

/// A short successful turn: one assistant delta, then completed.
pub(crate) fn plain_text_script() -> Vec<HarnessEvent> {
    vec![
        HarnessEvent::SessionStarted {
            harness_kind: HarnessKind::ClaudeCode,
            harness_version: "scripted".into(),
            resume_ref: Some("scripted-session".into()),
        },
        HarnessEvent::TurnStarted,
        HarnessEvent::AssistantDelta {
            text: "hello from the scripted engine".into(),
        },
        HarnessEvent::TurnCompleted {
            usage: tidebreak_core::CodeUsage {
                input_tokens: 4,
                output_tokens: 6,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
                context_tokens: 4,
            },
        },
    ]
}
