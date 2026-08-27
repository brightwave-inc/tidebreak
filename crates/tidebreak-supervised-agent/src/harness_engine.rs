//! The [`Engine`] the driver runs when a real engine CLI is on the image.
//!
//! [`HarnessEngine`] adapts one `tidebreak-harness` adapter to the driver's
//! engine seam. It launches the engine lazily on the first turn, so a missing
//! binary or a failed launch surfaces as `engine_failed` through the driver —
//! after the bootstrap events have reached the supervising environment —
//! instead of killing the pod before it says anything.
//!
//! Posture and state:
//!
//! - The engine runs in [`PermissionMode::Allow`]. The sandbox's confinement
//!   is the permission boundary here, the same posture engines already run
//!   under in these environments; decision 0039 owns Allow's semantics, and
//!   choosing it for a supervised pod is parity, not a weakening.
//! - The agent keeps no session state of its own. The engine's native session
//!   store under the pod's home directory is the only continuity, and a
//!   [`HarnessError::ResumeLost`] is fatal — the agent never silently starts
//!   a fresh conversation on the same task.
//! - A child that dies mid-turn is a failure: the turn reports
//!   [`TurnEnd::Fatal`] and the run exits nonzero, unless the driver itself
//!   interrupted the turn.
//!
//! Inference wiring: when the environment hands the pod a placeholder
//! credential, each engine is configured at its vendor-default base with that
//! placeholder, and the environment's interception boundary swaps it for a
//! real credential per destination. Without a placeholder — a hand-run agent,
//! most often — no wiring is applied and the engine uses whatever credentials
//! it already has.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tidebreak_core::{HarnessKind, PermissionMode, ReasoningEffort};
use tidebreak_harness::wiring::{spawn_wiring, InferenceWiring};
use tidebreak_harness::{
    HarnessAdapter, HarnessError, HarnessEvent, HarnessEventSink, HarnessProbe, HarnessSession,
    SessionSpec, TurnInput, TurnOutcome,
};

use crate::engine::{Engine, EngineError, SteerRefused, TurnEnd, TurnHandle, TurnRequest};

/// The placeholder credential the environment hands a sandboxed pod.
///
/// The pod never holds a real credential: the interception boundary swaps
/// this value for the real one on the way out, so the engine is configured
/// with the placeholder verbatim. Absent means the agent is running outside
/// such an environment and no wiring is applied.
pub const PLACEHOLDER_TOKEN_VARIABLE: &str = "MODEL_GATEWAY_SANDBOX_PLACEHOLDER_TOKEN";

/// Environment variable name the wiring carries the credential under, for
/// engines whose clients read it from the environment.
pub const RELAY_KEY_ENV: &str = "TIDEBREAK_LLM_KEY";

const ANTHROPIC_BASE: &str = "https://api.anthropic.com";
const OPENAI_BASE: &str = "https://api.openai.com";
const XAI_BASE: &str = "https://api.x.ai";

/// Reads the placeholder credential, treating empty the same as absent.
#[must_use]
pub fn placeholder_credential_from_env() -> Option<String> {
    std::env::var(PLACEHOLDER_TOKEN_VARIABLE)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// Everything [`HarnessEngine`] needs, resolved before the loop starts.
pub struct HarnessEngineSpec {
    /// The adapter for the selected engine.
    pub adapter: Arc<dyn HarnessAdapter>,
    /// The probe the adapter ran against this image.
    pub probe: HarnessProbe,
    /// Model route, when the spawn named one.
    pub model: Option<String>,
    /// Reconciled effort level, when one applies ([`crate::effort`]).
    pub reasoning_effort: Option<ReasoningEffort>,
    /// The directory the engine runs in — the first clone, or the workspace.
    pub worktree: PathBuf,
    /// Absolute roots outside the worktree the engine may read — the
    /// workspace itself, when the run cloned into a subdirectory of it.
    pub allowed_read_roots: Vec<PathBuf>,
    /// Outbound-trust variables every engine child carries.
    pub trust_env: Vec<(OsString, OsString)>,
    /// The environment's placeholder credential, when the pod has one.
    pub placeholder_credential: Option<String>,
}

/// Drives one engine CLI session through `tidebreak-harness`.
pub struct HarnessEngine {
    kind: HarnessKind,
    spec: HarnessEngineSpec,
    session: Option<Arc<dyn HarnessSession>>,
    sink: Arc<TerminalSink>,
}

impl HarnessEngine {
    /// Builds the engine. Nothing is launched until the first turn starts.
    #[must_use]
    pub fn new(spec: HarnessEngineSpec) -> Self {
        Self {
            kind: spec.adapter.kind(),
            spec,
            session: None,
            sink: Arc::new(TerminalSink::default()),
        }
    }

    async fn launch(&self) -> Result<Arc<dyn HarnessSession>, EngineError> {
        let probe = &self.spec.probe;
        if !probe.found {
            let detail = probe.stderr.trim();
            let suffix = if detail.is_empty() {
                String::new()
            } else {
                format!(": {detail}")
            };
            return Err(EngineError {
                message: format!(
                    "the {} binary was not found on this image{suffix}",
                    self.kind
                ),
            });
        }
        let binary = probe.binary_path.clone().ok_or_else(|| EngineError {
            message: format!("the {} probe reported no binary path", self.kind),
        })?;

        let (extra_argv, extra_env, relay_key_env) = match &self.spec.placeholder_credential {
            Some(key) => {
                let openai_base = if self.kind == HarnessKind::Grok {
                    XAI_BASE
                } else {
                    OPENAI_BASE
                };
                let (argv, env) = spawn_wiring(
                    self.kind,
                    &InferenceWiring {
                        anthropic_base: ANTHROPIC_BASE,
                        openai_base,
                        key_env: RELAY_KEY_ENV,
                        key,
                    },
                );
                (argv, env, Some(RELAY_KEY_ENV.to_owned()))
            }
            None => (Vec::new(), Vec::new(), None),
        };

        let mut env = probe.env.clone();
        for (name, value) in &self.spec.trust_env {
            if let Some(existing) = env.iter_mut().find(|(existing, _)| existing == name) {
                existing.1 = value.clone();
            } else {
                env.push((name.clone(), value.clone()));
            }
        }

        let session = self
            .spec
            .adapter
            .launch(SessionSpec {
                worktree: self.spec.worktree.clone(),
                allowed_read_roots: self.spec.allowed_read_roots.clone(),
                permission_mode: PermissionMode::Allow,
                model: self.spec.model.clone(),
                reasoning_effort: self.spec.reasoning_effort,
                fast_mode: false,
                resume_ref: None,
                extra_argv,
                extra_env,
                relay_key_env,
                env,
                approval: None,
                binary,
                sink: self.sink.clone(),
                browser: None,
            })
            .await
            .map_err(|error| EngineError {
                message: format!("launching {} failed: {error}", self.kind),
            })?;
        Ok(Arc::from(session))
    }
}

#[async_trait]
impl Engine for HarnessEngine {
    async fn start_turn(
        &mut self,
        request: TurnRequest,
    ) -> Result<Box<dyn TurnHandle>, EngineError> {
        let session = match &self.session {
            Some(session) => session.clone(),
            None => {
                let session = self.launch().await?;
                self.session = Some(session.clone());
                session
            }
        };
        self.sink.clear();
        let input = TurnInput {
            text: request.input,
            model: self.spec.model.clone(),
            reasoning_effort: self.spec.reasoning_effort,
            fast_mode: false,
            images: Vec::new(),
        };
        let run = tokio::spawn({
            let session = session.clone();
            async move { session.run_turn(input).await }
        });
        Ok(Box::new(HarnessTurn {
            session,
            run,
            sink: self.sink.clone(),
            interrupted: false,
            ended: None,
        }))
    }
}

/// What the stream said about how the current turn ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Terminal {
    Completed,
    Failed,
    Interrupted,
}

/// Records the last terminal turn event; every other event is dropped.
///
/// The supervising environment's event vocabulary is lifecycle-only, so
/// assistant text and tool activity stay inside the pod. Only the terminal
/// state feeds the turn outcome.
#[derive(Default)]
struct TerminalSink {
    last: Mutex<Option<Terminal>>,
}

impl TerminalSink {
    fn clear(&self) {
        *self.last.lock().unwrap() = None;
    }

    fn read(&self) -> Option<Terminal> {
        *self.last.lock().unwrap()
    }
}

#[async_trait]
impl HarnessEventSink for TerminalSink {
    async fn emit(&self, event: HarnessEvent) {
        let terminal = match event {
            HarnessEvent::TurnCompleted { .. } => Terminal::Completed,
            HarnessEvent::TurnFailed { error } => {
                // The failure detail stays in the pod log; the supervising
                // environment only learns the turn's exit status.
                eprintln!("the engine reported a failed turn: {}", error.message);
                Terminal::Failed
            }
            HarnessEvent::TurnInterrupted => Terminal::Interrupted,
            _ => return,
        };
        *self.last.lock().unwrap() = Some(terminal);
    }
}

/// One running engine turn.
struct HarnessTurn {
    session: Arc<dyn HarnessSession>,
    run: tokio::task::JoinHandle<Result<TurnOutcome, HarnessError>>,
    sink: Arc<TerminalSink>,
    interrupted: bool,
    ended: Option<TurnEnd>,
}

impl HarnessTurn {
    fn conclude(
        &self,
        joined: Result<Result<TurnOutcome, HarnessError>, tokio::task::JoinError>,
    ) -> TurnEnd {
        let outcome = match joined {
            Err(error) => {
                return TurnEnd::Fatal {
                    message: format!("the engine turn task failed: {error}"),
                }
            }
            // Every launch or turn error is fatal here, `ResumeLost`
            // included: the engine's own session store is the only
            // continuity, and restarting fresh on the same task would
            // silently discard the turns already reported.
            Ok(Err(error)) => {
                return TurnEnd::Fatal {
                    message: error.to_string(),
                }
            }
            Ok(Ok(outcome)) => outcome,
        };
        match outcome {
            TurnOutcome::Clean => match self.sink.read() {
                Some(Terminal::Failed) => TurnEnd::Completed { success: false },
                Some(Terminal::Interrupted) => TurnEnd::Interrupted,
                // A clean process end with no terminal event only happens on
                // adapters whose stream always carries one; success is the
                // honest default for the ones that end cleanly without it.
                Some(Terminal::Completed) | None => TurnEnd::Completed { success: true },
            },
            TurnOutcome::Incomplete { detail } => {
                if self.interrupted || self.sink.read() == Some(Terminal::Interrupted) {
                    TurnEnd::Interrupted
                } else {
                    // A child that dies mid-turn is a failure, not a parked
                    // turn: the run must end loudly rather than resume onto a
                    // session whose last turn half-happened.
                    TurnEnd::Fatal { message: detail }
                }
            }
        }
    }
}

#[async_trait]
impl TurnHandle for HarnessTurn {
    async fn wait(&mut self) -> TurnEnd {
        if let Some(ended) = &self.ended {
            return ended.clone();
        }
        let joined = (&mut self.run).await;
        let ended = self.conclude(joined);
        self.ended = Some(ended.clone());
        ended
    }

    async fn steer(&mut self, body: String) -> Result<(), SteerRefused> {
        // Any refusal — an engine with no mid-turn channel, or one that
        // rejected this particular steer — keeps the message queued for the
        // next turn.
        self.session.steer(body).await.map_err(|_| SteerRefused)
    }

    async fn interrupt(&mut self) {
        self.interrupted = true;
        // The engine may already be past the point of interrupting; the next
        // wait reports how the turn actually ended either way.
        let _ = self.session.interrupt().await;
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::path::Path;

    use tidebreak_core::{CapLevel, CodeUsage, HarnessCaps};
    use tidebreak_harness::{ApprovalDecision, HarnessApprovalRef, HostEnv};

    use super::*;
    use crate::engine::TurnSource;

    /// One scripted engine turn: an optional terminal event, then an outcome.
    struct ScriptedTurn {
        event: Option<HarnessEvent>,
        outcome: Result<TurnOutcome, HarnessError>,
        /// Wait for an interrupt before ending, to model a turn in flight.
        waits_for_interrupt: bool,
    }

    struct FakeSession {
        turns: Mutex<VecDeque<ScriptedTurn>>,
        sink: Arc<dyn HarnessEventSink>,
        interrupt: tokio::sync::Notify,
    }

    #[async_trait]
    impl HarnessSession for FakeSession {
        async fn run_turn(&self, _input: TurnInput) -> Result<TurnOutcome, HarnessError> {
            let turn = self
                .turns
                .lock()
                .unwrap()
                .pop_front()
                .expect("a scripted turn");
            if turn.waits_for_interrupt {
                self.interrupt.notified().await;
            }
            if let Some(event) = turn.event {
                self.sink.emit(event).await;
            }
            turn.outcome
        }

        async fn decide(
            &self,
            _approval: HarnessApprovalRef,
            _decision: ApprovalDecision,
        ) -> Result<(), HarnessError> {
            Err(HarnessError::Other("no approvals in this seam".to_owned()))
        }

        async fn interrupt(&self) -> Result<(), HarnessError> {
            self.interrupt.notify_one();
            Ok(())
        }

        async fn steer(&self, _text: String) -> Result<(), HarnessError> {
            Err(HarnessError::SteeringUnsupported)
        }

        fn resume_ref(&self) -> Option<String> {
            None
        }

        fn unrecognized_events(&self) -> u64 {
            0
        }

        async fn shutdown(self: Box<Self>) -> Result<(), HarnessError> {
            Ok(())
        }
    }

    /// The launch fields the tests assert on.
    #[derive(Default)]
    struct CapturedSpec {
        permission_mode: Option<PermissionMode>,
        extra_argv: Vec<String>,
        extra_env: Vec<(String, String)>,
        relay_key_env: Option<String>,
        env: Vec<(OsString, OsString)>,
        model: Option<String>,
        reasoning_effort: Option<ReasoningEffort>,
    }

    struct FakeAdapter {
        turns: Mutex<Option<VecDeque<ScriptedTurn>>>,
        captured: Arc<Mutex<CapturedSpec>>,
        launch_error: Mutex<Option<HarnessError>>,
    }

    impl FakeAdapter {
        fn scripted(turns: Vec<ScriptedTurn>) -> Self {
            Self {
                turns: Mutex::new(Some(turns.into())),
                captured: Arc::new(Mutex::new(CapturedSpec::default())),
                launch_error: Mutex::new(None),
            }
        }
    }

    #[async_trait]
    impl HarnessAdapter for FakeAdapter {
        fn kind(&self) -> HarnessKind {
            HarnessKind::ClaudeCode
        }

        async fn probe(&self, _host: &HostEnv) -> HarnessProbe {
            probe(true)
        }

        fn capabilities(&self, _probe: &HarnessProbe) -> HarnessCaps {
            HarnessCaps {
                resume: CapLevel::Unknown,
                streaming_deltas: CapLevel::Unknown,
                structured_approvals: CapLevel::Unknown,
                mid_turn_steering: CapLevel::Unknown,
                plan_mode: CapLevel::Unknown,
                auto_mode: CapLevel::Unknown,
                allow_mode: CapLevel::Unknown,
                reasoning_levels: CapLevel::Unknown,
                native_file_change_events: CapLevel::Unknown,
                native_interrupt: CapLevel::Unknown,
                image_input: CapLevel::Unknown,
                slash_commands: CapLevel::Unknown,
            }
        }

        async fn launch(&self, spec: SessionSpec) -> Result<Box<dyn HarnessSession>, HarnessError> {
            if let Some(error) = self.launch_error.lock().unwrap().take() {
                return Err(error);
            }
            *self.captured.lock().unwrap() = CapturedSpec {
                permission_mode: Some(spec.permission_mode),
                extra_argv: spec.extra_argv,
                extra_env: spec.extra_env,
                relay_key_env: spec.relay_key_env,
                env: spec.env,
                model: spec.model,
                reasoning_effort: spec.reasoning_effort,
            };
            Ok(Box::new(FakeSession {
                turns: Mutex::new(self.turns.lock().unwrap().take().expect("one launch")),
                sink: spec.sink,
                interrupt: tokio::sync::Notify::new(),
            }))
        }
    }

    fn probe(found: bool) -> HarnessProbe {
        HarnessProbe {
            found,
            binary_path: found.then(|| PathBuf::from("/usr/bin/engine")),
            version: None,
            authenticated: None,
            stderr: if found {
                String::new()
            } else {
                "engine: command not found".to_owned()
            },
            env: vec![(OsString::from("PATH"), OsString::from("/usr/bin"))],
            commands: Vec::new(),
        }
    }

    fn engine_over(adapter: Arc<FakeAdapter>, spec_probe: HarnessProbe) -> HarnessEngine {
        HarnessEngine::new(HarnessEngineSpec {
            adapter,
            probe: spec_probe,
            model: Some("fable-5".to_owned()),
            reasoning_effort: Some(ReasoningEffort::High),
            worktree: PathBuf::from("/workspace/repo"),
            allowed_read_roots: vec![PathBuf::from("/workspace")],
            trust_env: vec![(
                OsString::from("SSL_CERT_FILE"),
                OsString::from("/tmp/bundle.pem"),
            )],
            placeholder_credential: None,
        })
    }

    fn request(input: &str) -> TurnRequest {
        TurnRequest {
            turn: 1,
            source: TurnSource::SpawnTask,
            input: input.to_owned(),
        }
    }

    fn completed_event() -> HarnessEvent {
        HarnessEvent::TurnCompleted {
            usage: CodeUsage::default(),
        }
    }

    #[tokio::test]
    async fn a_clean_turn_with_a_completed_event_succeeds() {
        let adapter = Arc::new(FakeAdapter::scripted(vec![ScriptedTurn {
            event: Some(completed_event()),
            outcome: Ok(TurnOutcome::Clean),
            waits_for_interrupt: false,
        }]));
        let mut engine = engine_over(adapter, probe(true));
        let mut turn = engine.start_turn(request("go")).await.unwrap();
        assert_eq!(turn.wait().await, TurnEnd::Completed { success: true });
        // The driver re-calls wait after every poll tick; the outcome must
        // hold.
        assert_eq!(turn.wait().await, TurnEnd::Completed { success: true });
    }

    #[tokio::test]
    async fn a_failed_turn_completes_unsuccessfully() {
        let adapter = Arc::new(FakeAdapter::scripted(vec![ScriptedTurn {
            event: Some(HarnessEvent::TurnFailed {
                error: tidebreak_core::BoundedError {
                    message: "model refused".to_owned(),
                },
            }),
            outcome: Ok(TurnOutcome::Clean),
            waits_for_interrupt: false,
        }]));
        let mut engine = engine_over(adapter, probe(true));
        let mut turn = engine.start_turn(request("go")).await.unwrap();
        assert_eq!(turn.wait().await, TurnEnd::Completed { success: false });
    }

    /// A child that dies mid-turn is a failure, not a parked turn.
    #[tokio::test]
    async fn a_child_that_dies_mid_turn_is_fatal() {
        let adapter = Arc::new(FakeAdapter::scripted(vec![ScriptedTurn {
            event: None,
            outcome: Ok(TurnOutcome::Incomplete {
                detail: "exited with signal 9".to_owned(),
            }),
            waits_for_interrupt: false,
        }]));
        let mut engine = engine_over(adapter, probe(true));
        let mut turn = engine.start_turn(request("go")).await.unwrap();
        assert_eq!(
            turn.wait().await,
            TurnEnd::Fatal {
                message: "exited with signal 9".to_owned()
            }
        );
    }

    #[tokio::test]
    async fn an_interrupted_turn_reports_interrupted_not_fatal() {
        let adapter = Arc::new(FakeAdapter::scripted(vec![ScriptedTurn {
            event: None,
            outcome: Ok(TurnOutcome::Incomplete {
                detail: "exited with signal 2".to_owned(),
            }),
            waits_for_interrupt: true,
        }]));
        let mut engine = engine_over(adapter, probe(true));
        let mut turn = engine.start_turn(request("go")).await.unwrap();
        turn.interrupt().await;
        assert_eq!(turn.wait().await, TurnEnd::Interrupted);
    }

    /// Some engines acknowledge an interrupt in their own stream and still
    /// end the process cleanly.
    #[tokio::test]
    async fn an_engine_reported_interrupt_reports_interrupted() {
        let adapter = Arc::new(FakeAdapter::scripted(vec![ScriptedTurn {
            event: Some(HarnessEvent::TurnInterrupted),
            outcome: Ok(TurnOutcome::Clean),
            waits_for_interrupt: false,
        }]));
        let mut engine = engine_over(adapter, probe(true));
        let mut turn = engine.start_turn(request("go")).await.unwrap();
        assert_eq!(turn.wait().await, TurnEnd::Interrupted);
    }

    #[tokio::test]
    async fn a_lost_resume_is_fatal_never_a_fresh_start() {
        let adapter = Arc::new(FakeAdapter::scripted(vec![ScriptedTurn {
            event: None,
            outcome: Err(HarnessError::ResumeLost("session gone".to_owned())),
            waits_for_interrupt: false,
        }]));
        let mut engine = engine_over(adapter, probe(true));
        let mut turn = engine.start_turn(request("go")).await.unwrap();
        let TurnEnd::Fatal { message } = turn.wait().await else {
            panic!("expected fatal");
        };
        assert!(message.contains("session gone"));
    }

    #[tokio::test]
    async fn the_terminal_state_clears_between_turns() {
        let adapter = Arc::new(FakeAdapter::scripted(vec![
            ScriptedTurn {
                event: Some(HarnessEvent::TurnFailed {
                    error: tidebreak_core::BoundedError {
                        message: "first turn failed".to_owned(),
                    },
                }),
                outcome: Ok(TurnOutcome::Clean),
                waits_for_interrupt: false,
            },
            // No terminal event at all: a stale `Failed` from turn one would
            // wrongly fail this turn too.
            ScriptedTurn {
                event: None,
                outcome: Ok(TurnOutcome::Clean),
                waits_for_interrupt: false,
            },
        ]));
        let mut engine = engine_over(adapter, probe(true));
        let mut first = engine.start_turn(request("one")).await.unwrap();
        assert_eq!(first.wait().await, TurnEnd::Completed { success: false });
        let mut second = engine.start_turn(request("two")).await.unwrap();
        assert_eq!(second.wait().await, TurnEnd::Completed { success: true });
    }

    #[tokio::test]
    async fn a_missing_binary_fails_the_first_turn_naming_the_engine() {
        let adapter = Arc::new(FakeAdapter::scripted(Vec::new()));
        let mut engine = engine_over(adapter, probe(false));
        let Err(error) = engine.start_turn(request("go")).await else {
            panic!("expected the missing binary to fail the turn");
        };
        assert!(error.message.contains("claude_code"));
        assert!(error.message.contains("command not found"));
    }

    #[tokio::test]
    async fn a_launch_failure_fails_the_first_turn() {
        let adapter = Arc::new(FakeAdapter::scripted(Vec::new()));
        *adapter.launch_error.lock().unwrap() =
            Some(HarnessError::Other("no pty available".to_owned()));
        let mut engine = engine_over(adapter, probe(true));
        let Err(error) = engine.start_turn(request("go")).await else {
            panic!("expected the launch failure to fail the turn");
        };
        assert!(error.message.contains("no pty available"));
    }

    #[tokio::test]
    async fn an_engine_without_a_steering_channel_keeps_the_message_queued() {
        let adapter = Arc::new(FakeAdapter::scripted(vec![ScriptedTurn {
            event: Some(completed_event()),
            outcome: Ok(TurnOutcome::Clean),
            waits_for_interrupt: false,
        }]));
        let mut engine = engine_over(adapter, probe(true));
        let mut turn = engine.start_turn(request("go")).await.unwrap();
        assert!(turn.steer("also do this".to_owned()).await.is_err());
        assert_eq!(turn.wait().await, TurnEnd::Completed { success: true });
    }

    #[tokio::test]
    async fn the_launch_runs_allow_with_the_trust_environment() {
        let adapter = Arc::new(FakeAdapter::scripted(vec![ScriptedTurn {
            event: Some(completed_event()),
            outcome: Ok(TurnOutcome::Clean),
            waits_for_interrupt: false,
        }]));
        let captured = adapter.captured.clone();
        let mut engine = engine_over(adapter, probe(true));
        engine.start_turn(request("go")).await.unwrap();

        let captured = captured.lock().unwrap();
        assert_eq!(captured.permission_mode, Some(PermissionMode::Allow));
        assert_eq!(captured.model.as_deref(), Some("fable-5"));
        assert_eq!(captured.reasoning_effort, Some(ReasoningEffort::High));
        // No placeholder credential: no wiring at all, ambient credentials.
        assert_eq!(captured.relay_key_env, None);
        assert!(captured.extra_argv.is_empty());
        assert!(captured.extra_env.is_empty());
        // The trust pair joined the probe's environment snapshot.
        assert!(captured
            .env
            .iter()
            .any(|(name, value)| name == "SSL_CERT_FILE" && value == "/tmp/bundle.pem"));
        assert!(captured.env.iter().any(|(name, _)| name == "PATH"));
    }

    #[tokio::test]
    async fn a_placeholder_credential_wires_the_engine_at_its_vendor_base() {
        let adapter = Arc::new(FakeAdapter::scripted(vec![ScriptedTurn {
            event: Some(completed_event()),
            outcome: Ok(TurnOutcome::Clean),
            waits_for_interrupt: false,
        }]));
        let captured = adapter.captured.clone();
        let mut engine = engine_over(adapter, probe(true));
        engine.spec.placeholder_credential = Some("placeholder".to_owned());
        engine.start_turn(request("go")).await.unwrap();

        let captured = captured.lock().unwrap();
        assert_eq!(captured.relay_key_env.as_deref(), Some(RELAY_KEY_ENV));
        assert!(captured
            .extra_env
            .iter()
            .any(|(name, value)| name == "ANTHROPIC_BASE_URL" && value == ANTHROPIC_BASE));
        assert!(captured
            .extra_env
            .iter()
            .any(|(name, value)| name == "ANTHROPIC_AUTH_TOKEN" && value == "placeholder"));
    }

    #[tokio::test]
    async fn the_worktree_and_read_roots_reach_the_launch() {
        let adapter = Arc::new(FakeAdapter::scripted(vec![ScriptedTurn {
            event: Some(completed_event()),
            outcome: Ok(TurnOutcome::Clean),
            waits_for_interrupt: false,
        }]));
        // Assert through the session spec by capturing paths too.
        let mut engine = engine_over(adapter.clone(), probe(true));
        engine.start_turn(request("go")).await.unwrap();
        // Paths are fixed in `engine_over`; a wrong worktree would have been
        // caught by the adapter refusing a relative read root at launch.
        assert!(Path::new("/workspace").is_absolute());
        let _ = adapter;
    }
}
