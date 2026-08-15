//! A [`HarnessAdapter`] / [`HarnessSession`] driven by a script of events.
//!
//! Compiled only under the `scripted-harness` feature or in this crate's
//! tests, matching [`scripted_provider`]: a released binary never contains it.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tidebreak_core::{CapLevel, CodePermissionMode, HarnessCaps, HarnessKind};
use tidebreak_harness::{
    ApprovalCompleter, ApprovalDecision, HarnessAdapter, HarnessApprovalRef, HarnessError,
    HarnessEvent, HarnessEventSink, HarnessProbe, HarnessSession, HostEnv, SessionSpec, TurnInput,
};
use tokio::sync::{oneshot, Mutex};

/// Completer that records decisions and unparks a scripted turn.
#[derive(Default)]
pub(crate) struct ScriptedApprover {
    parked: std::sync::Mutex<Option<oneshot::Sender<ApprovalDecision>>>,
    observed: std::sync::Mutex<Vec<(String, ApprovalDecision)>>,
}

impl ScriptedApprover {
    pub(crate) fn park(&self) -> oneshot::Receiver<ApprovalDecision> {
        let (tx, rx) = oneshot::channel();
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
    structured_approvals: CapLevel,
    child_pid: Option<i64>,
    approver: Arc<ScriptedApprover>,
}

impl ScriptedAdapter {
    pub(crate) fn new(events: Vec<HarnessEvent>) -> Self {
        Self {
            kind: HarnessKind::ClaudeCode,
            events,
            delay: Duration::ZERO,
            mid_turn_steering: CapLevel::Unsupported,
            structured_approvals: CapLevel::Unsupported,
            child_pid: None,
            approver: Arc::new(ScriptedApprover::default()),
        }
    }

    pub(crate) fn with_approvals(mut self, level: CapLevel) -> Self {
        self.structured_approvals = level;
        self
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

    #[allow(dead_code)]
    pub(crate) fn with_child_pid(mut self, pid: i64) -> Self {
        self.child_pid = Some(pid);
        self
    }
}

#[async_trait]
impl HarnessAdapter for ScriptedAdapter {
    fn kind(&self) -> HarnessKind {
        self.kind
    }

    async fn probe(&self, _host: &HostEnv) -> HarnessProbe {
        HarnessProbe {
            found: true,
            binary_path: Some(PathBuf::from("/scripted/engine")),
            version: Some("scripted".into()),
            authenticated: Some(true),
            stderr: String::new(),
            env: Vec::new(),
        }
    }

    fn capabilities(&self, _probe: &HarnessProbe) -> HarnessCaps {
        HarnessCaps {
            resume: CapLevel::Supported,
            streaming_deltas: CapLevel::Supported,
            structured_approvals: self.structured_approvals,
            mid_turn_steering: self.mid_turn_steering,
            plan_mode: CapLevel::Supported,
            reasoning_levels: CapLevel::Unsupported,
            native_file_change_events: CapLevel::Unsupported,
            native_interrupt: CapLevel::Supported,
        }
    }

    async fn launch(&self, spec: SessionSpec) -> Result<Box<dyn HarnessSession>, HarnessError> {
        if spec.permission_mode != CodePermissionMode::Plan
            && spec.permission_mode != CodePermissionMode::Ask
            && spec.permission_mode != CodePermissionMode::Auto
        {
            return Err(HarnessError::PermissionModeUnsupported(
                spec.permission_mode,
            ));
        }
        Ok(Box::new(ScriptedSession {
            sink: spec.sink,
            events: self.events.clone(),
            delay: self.delay,
            mid_turn_steering: self.mid_turn_steering,
            child_pid: self.child_pid,
            interrupt: Arc::new(AtomicBool::new(false)),
            steered: Mutex::new(None),
            unrecognized: AtomicU64::new(0),
            approver: self.approver.clone(),
        }))
    }
}

struct ScriptedSession {
    sink: Arc<dyn HarnessEventSink>,
    events: Vec<HarnessEvent>,
    delay: Duration,
    mid_turn_steering: CapLevel,
    child_pid: Option<i64>,
    interrupt: Arc<AtomicBool>,
    steered: Mutex<Option<String>>,
    unrecognized: AtomicU64,
    approver: Arc<ScriptedApprover>,
}

#[async_trait]
impl HarnessSession for ScriptedSession {
    async fn run_turn(&mut self, _input: TurnInput) -> Result<(), HarnessError> {
        self.interrupt.store(false, Ordering::SeqCst);
        for event in &self.events {
            if self.interrupt.load(Ordering::SeqCst) {
                self.sink.emit(HarnessEvent::TurnInterrupted).await;
                return Ok(());
            }
            if !self.delay.is_zero() {
                tokio::time::sleep(self.delay).await;
            }
            if let Some(text) = self.steered.lock().await.take() {
                self.sink.emit(HarnessEvent::UserSteered { text }).await;
            }
            if let HarnessEvent::ApprovalRequested { harness_ref, .. } = event {
                self.sink.emit(event.clone()).await;
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
                return Ok(());
            }
        }
        if self.interrupt.load(Ordering::SeqCst) {
            self.sink.emit(HarnessEvent::TurnInterrupted).await;
        }
        Ok(())
    }

    async fn decide(
        &self,
        approval: HarnessApprovalRef,
        decision: ApprovalDecision,
    ) -> Result<(), HarnessError> {
        self.approver.complete(&approval.call_id, decision).await
    }

    fn approval_completer(&self) -> Arc<dyn ApprovalCompleter> {
        self.approver.clone()
    }

    async fn interrupt(&mut self) -> Result<(), HarnessError> {
        self.interrupt.store(true, Ordering::SeqCst);
        Ok(())
    }

    async fn steer(&mut self, text: String) -> Result<(), HarnessError> {
        if self.mid_turn_steering != CapLevel::Supported {
            return Err(HarnessError::Other(
                "mid-turn steering is not available on this engine".into(),
            ));
        }
        *self.steered.lock().await = Some(text);
        Ok(())
    }

    fn resume_ref(&self) -> Option<String> {
        None
    }

    fn child_pid(&self) -> Option<i64> {
        self.child_pid
    }

    async fn shutdown(self: Box<Self>) -> Result<(), HarnessError> {
        let _ = self.unrecognized;
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
            },
        },
    ]
}
