//! Protocol translation from an external agent engine into one normalized
//! event vocabulary.
//!
//! Nothing in this crate's traits assumes the engine is a coding agent.
//! Tidebreak's own internal loop is a future implementor of the same
//! contract. Orchestration, persistence, and UI consume only
//! [`tidebreak_core::CodeEvent`]; this crate emits the unpersisted sibling
//! [`HarnessEvent`].

#![deny(missing_docs)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tidebreak_core::{
    BoundedError, CodePermissionMode, CodeUsage, Diffstat, FileChangeKind, HarnessCaps,
    HarnessKind, HarnessNoticeLevel, ToolDetail, ToolOutcome,
};

pub mod budget;
pub mod child;
pub mod claude;
pub mod codex;
pub mod grok;
pub mod launch;
pub mod opencode;
pub mod probe;
mod text;

pub use budget::{BudgetTick, StreamBudget, StreamLineBuffer};
pub use child::ChildPid;
pub use launch::{validate_launch_plan, BypassFlagError, LaunchPlan};
pub use probe::{
    filter_child_env, observe_version, probe_shell, resolve_binary, HostEnv, ProbeCapture,
    ProbeError,
};

/// Normalized, unpersisted event. Maps 1:1 onto [`tidebreak_core::CodeEvent`]
/// minus persistence ids (turn id, approval id).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HarnessEvent {
    /// The engine session has started (or resumed).
    SessionStarted {
        /// Which engine.
        harness_kind: HarnessKind,
        /// Version observed at launch.
        harness_version: String,
        /// Engine-native resume token, when the stream reported one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resume_ref: Option<String>,
    },
    /// A user turn has begun on the engine side.
    TurnStarted,
    /// A chunk of assistant text.
    AssistantDelta {
        /// The text fragment.
        text: String,
    },
    /// A completed assistant message.
    AssistantMessage {
        /// The message text.
        text: String,
    },
    /// A chunk of reasoning text.
    ReasoningDelta {
        /// The reasoning fragment.
        text: String,
    },
    /// The engine has begun a tool call.
    ToolStarted {
        /// Engine-native call id.
        call_id: String,
        /// Tool name.
        name: String,
        /// Display-oriented classification.
        detail: ToolDetail,
    },
    /// A tool call finished.
    ToolCompleted {
        /// Engine-native call id.
        call_id: String,
        /// How it finished.
        outcome: ToolOutcome,
        /// Bounded preview.
        preview: String,
    },
    /// A file changed.
    FileChanged {
        /// Path.
        path: String,
        /// Kind of change.
        kind: FileChangeKind,
        /// Bounded diffstat.
        diffstat: Diffstat,
    },
    /// The engine asked for an approval.
    ApprovalRequested {
        /// Engine-native handle used to decide the request.
        harness_ref: HarnessApprovalRef,
        /// Size-capped raw engine payload. Null when the engine emitted a
        /// request without a captured body.
        #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
        raw: serde_json::Value,
    },
    /// An approval was decided (observed on the stream, or after [`HarnessSession::decide`]).
    ApprovalResolved {
        /// Engine-native handle.
        harness_ref: HarnessApprovalRef,
        /// The decision.
        decision: ApprovalDecision,
    },
    /// The user injected a mid-turn message.
    UserSteered {
        /// The steered text.
        text: String,
    },
    /// The turn finished successfully.
    TurnCompleted {
        /// Token accounting as reported by the engine.
        usage: CodeUsage,
    },
    /// The turn failed.
    TurnFailed {
        /// Bounded error.
        error: BoundedError,
    },
    /// The turn was interrupted.
    TurnInterrupted,
    /// Visible degradation or an engine-native notice.
    HarnessNotice {
        /// Severity.
        level: HarnessNoticeLevel,
        /// Bounded message.
        message: String,
    },
}

/// Engine-native handle for a parked approval.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct HarnessApprovalRef {
    /// Call or request id the engine will recognize on decide.
    pub call_id: String,
}

/// Decision returned through the engine's native approval channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ApprovalDecision {
    /// Approve the request.
    Approve,
    /// Deny, optionally with steering feedback the engine surfaces to the model.
    Deny {
        /// Feedback, when any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        feedback: Option<String>,
    },
}

/// One user turn to feed the engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnInput {
    /// The user's message.
    pub text: String,
}

/// How the engine process behind one turn ended.
///
/// An adapter that runs one child per turn must report the child's exit here:
/// stdout reaching EOF says only that the pipe closed, and a killed, crashed,
/// or signed-out engine reaches EOF exactly like a finished one. The worker,
/// not the adapter, decides how to close the turn — it is the side that knows
/// whether the stop was asked for.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum TurnOutcome {
    /// A terminal turn event arrived, and any per-turn child exited
    /// successfully. Adapters with a long-lived child (or none) report this.
    #[default]
    Clean,
    /// The engine ended without a terminal turn event, exited non-zero, or
    /// was signaled.
    Incomplete {
        /// Bounded description — exit code or signal, plus captured stderr.
        /// Safe to journal.
        detail: String,
    },
}

/// Completes a parked engine approval. Implemented by the server bridge so
/// [`HarnessSession::decide`] can run while `run_turn` is blocked on the child.
#[async_trait]
pub trait ApprovalCompleter: Send + Sync {
    /// Finish the native channel for `call_id`.
    async fn complete(&self, call_id: &str, decision: ApprovalDecision)
        -> Result<(), HarnessError>;
}

/// Loopback approval-channel wiring supplied by the server layer.
///
/// The server must serve a permission-prompt tool at `mcp_endpoint_url`
/// authenticated by `token`. This crate does not implement that endpoint.
#[derive(Clone)]
pub struct ApprovalChannelSpec {
    /// Loopback MCP endpoint URL.
    pub mcp_endpoint_url: String,
    /// Session-scoped token. Never logged.
    pub token: String,
    /// How [`HarnessSession::decide`] finishes the parked MCP call.
    pub completer: Arc<dyn ApprovalCompleter>,
}

impl std::fmt::Debug for ApprovalChannelSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApprovalChannelSpec")
            .field("mcp_endpoint_url", &self.mcp_endpoint_url)
            .field("token", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl ApprovalChannelSpec {
    /// `--mcp-config` JSON captured for Claude Code 2.1.233 HTTP MCP.
    #[must_use]
    pub fn mcp_config_json(&self, server_name: &str) -> String {
        serde_json::json!({
            "mcpServers": {
                server_name: {
                    "type": "http",
                    "url": self.mcp_endpoint_url,
                    "headers": {
                        "Authorization": format!("Bearer {}", self.token),
                    },
                }
            }
        })
        .to_string()
    }
}

/// What an adapter needs to spawn or connect one session.
pub struct SessionSpec {
    /// Worktree the engine should use as its working directory.
    pub worktree: PathBuf,
    /// Permission mode. Adapters refuse a mode they cannot honor.
    pub permission_mode: CodePermissionMode,
    /// Engine-native resume token, when continuing a prior session.
    pub resume_ref: Option<String>,
    /// Extra argv from settings. Still subject to the bypass-flag denylist.
    pub extra_argv: Vec<String>,
    /// Extra environment from settings. Cannot override adapter-owned keys.
    pub extra_env: Vec<(String, String)>,
    /// Shell-resolved environment captured by the probe. Children run under
    /// this snapshot, not the GUI process environment.
    pub env: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    /// Approval-channel wiring, when the server has one to offer.
    pub approval: Option<ApprovalChannelSpec>,
    /// Absolute engine binary, already resolved by [`probe`].
    pub binary: PathBuf,
    /// Where normalized events go.
    pub sink: Arc<dyn HarnessEventSink>,
}

/// Receives normalized events as the engine stream is parsed.
#[async_trait]
pub trait HarnessEventSink: Send + Sync {
    /// Emit one event.
    async fn emit(&self, event: HarnessEvent);
}

/// Adapter for one external agent engine.
#[async_trait]
pub trait HarnessAdapter: Send + Sync {
    /// Which engine this adapter drives.
    fn kind(&self) -> HarnessKind;

    /// Login-shell PATH resolution, version detection, auth observation.
    /// Never reads or stores credentials.
    async fn probe(&self, host: &HostEnv) -> HarnessProbe;

    /// Every capability flag stated for the probed version. `Unknown` is
    /// legal; silence is not.
    fn capabilities(&self, probe: &HarnessProbe) -> HarnessCaps;

    /// Spawn or connect for one session.
    async fn launch(&self, spec: SessionSpec) -> Result<Box<dyn HarnessSession>, HarnessError>;
}

/// One live engine session.
///
/// Control methods take shared `&self` so a session worker can dispatch
/// [`Self::decide`] and [`Self::interrupt`] while [`Self::run_turn`] is still
/// in flight. Adapters keep process state behind interior mutability.
#[async_trait]
pub trait HarnessSession: Send + Sync {
    /// Feed one user turn; normalized events flow to the sink until a
    /// terminal turn event arrives.
    ///
    /// The returned [`TurnOutcome`] is how the *process* ended, which the
    /// stream alone cannot say. Returning [`TurnOutcome::Clean`] for a child
    /// that died is how an interrupted or crashed turn ends up journaled as a
    /// success.
    async fn run_turn(&self, input: TurnInput) -> Result<TurnOutcome, HarnessError>;

    /// Resolve a pending approval through the engine's native channel.
    async fn decide(
        &self,
        approval: HarnessApprovalRef,
        decision: ApprovalDecision,
    ) -> Result<(), HarnessError>;

    /// Ask the engine to stop the current turn.
    async fn interrupt(&self) -> Result<(), HarnessError>;

    /// Inject a mid-turn user message, when the engine accepts one.
    ///
    /// The default refuses: an adapter must override this only when its
    /// capability vector states [`CapLevel::Supported`] for mid-turn steering.
    async fn steer(&self, text: String) -> Result<(), HarnessError> {
        let _ = text;
        Err(HarnessError::Other(
            "mid-turn steering is not available on this engine".into(),
        ))
    }

    /// Engine-native resume token, when the stream has reported one.
    ///
    /// Report a token only once it would actually resume: the caller persists
    /// what this returns and hands it back on the next launch, so a token the
    /// engine has not committed yet must stay unreported until it has.
    fn resume_ref(&self) -> Option<String>;

    /// Child pid recorded from a process this session spawned, when any.
    ///
    /// Recovery only ever probes a pid this method has exposed. The default
    /// is `None`: an adapter that does not spawn a child, or has not spawned
    /// one yet, must not invent a pid.
    fn child_pid(&self) -> Option<i64> {
        None
    }

    /// Every transition of [`Self::child_pid`], for a watcher that must
    /// record the pid while a turn is in flight.
    ///
    /// An adapter that spawns a child *per turn* has no pid to report at turn
    /// boundaries — which is precisely the window a crash orphans a child in.
    /// Such adapters publish here the moment the child exists and clear it
    /// when the child exits. The default is `None`: an adapter whose pid is
    /// already stable across the session has nothing to stream.
    fn child_pid_changes(&self) -> Option<tokio::sync::watch::Receiver<Option<i64>>> {
        None
    }

    /// How many stream events this session could not map to a
    /// [`HarnessEvent`], counted since it was launched.
    ///
    /// Monotonic for the life of the session; the worker flushes the delta
    /// onto the session row after each turn so the count survives a restart.
    /// Deliberately has no default: an adapter that dropped part of a stream
    /// must say so rather than inherit a silent zero (decision 0031).
    fn unrecognized_events(&self) -> u64;

    /// Tear the session down.
    async fn shutdown(self: Box<Self>) -> Result<(), HarnessError>;
}

/// Result of probing an installed engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessProbe {
    /// Whether an absolute, executable binary was found.
    pub found: bool,
    /// Resolved absolute path.
    pub binary_path: Option<PathBuf>,
    /// Detected version string, when `--version` (or equivalent) worked.
    pub version: Option<String>,
    /// Authentication observation: `Some(true)` signed in, `Some(false)`
    /// signed out, `None` not observed (the adapter must not guess).
    pub authenticated: Option<bool>,
    /// Bounded stderr from the probe, for the doctor surface.
    pub stderr: String,
    /// Shell-resolved environment captured by the same probe.
    pub env: Vec<(std::ffi::OsString, std::ffi::OsString)>,
}

/// Adapter failure.
#[derive(Debug, thiserror::Error)]
pub enum HarnessError {
    /// The engine binary is not installed or not executable.
    #[error("engine binary not found")]
    NotFound,
    /// A composed launch plan contained a forbidden flag.
    #[error(transparent)]
    LaunchRejected(#[from] BypassFlagError),
    /// The engine cannot honor the requested permission mode.
    #[error("permission mode {0} is not available on this engine")]
    PermissionModeUnsupported(CodePermissionMode),
    /// The engine no longer knows the session this spec asked to resume.
    ///
    /// The stored resume ref is dead: retrying with it fails identically
    /// forever, so the caller must drop it and start a fresh engine session
    /// rather than treat this as one failed turn.
    #[error("the engine no longer has this session: {0}")]
    ResumeLost(String),
    /// I/O or spawn failure.
    #[error("engine io: {0}")]
    Io(#[from] std::io::Error),
    /// Anything else, already bounded.
    #[error("{0}")]
    Other(String),
}

/// Registry of in-process adapters, keyed by [`HarnessKind`].
#[derive(Default)]
pub struct AdapterRegistry {
    adapters: BTreeMap<String, Arc<dyn HarnessAdapter>>,
}

impl AdapterRegistry {
    /// Empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an adapter, replacing any previous adapter for the same kind.
    pub fn register(&mut self, adapter: Arc<dyn HarnessAdapter>) {
        self.adapters
            .insert(adapter.kind().as_str().to_owned(), adapter);
    }

    /// Look up an adapter.
    #[must_use]
    pub fn get(&self, kind: HarnessKind) -> Option<Arc<dyn HarnessAdapter>> {
        self.adapters.get(kind.as_str()).cloned()
    }

    /// Every registered adapter, in kind-token order.
    pub fn iter(&self) -> impl Iterator<Item = Arc<dyn HarnessAdapter>> + '_ {
        self.adapters.values().cloned()
    }
}

/// Built-in adapters shipped with this crate.
#[must_use]
pub fn builtin_registry() -> AdapterRegistry {
    let mut registry = AdapterRegistry::new();
    registry.register(Arc::new(claude::ClaudeCodeAdapter::new()));
    registry.register(Arc::new(codex::CodexAdapter::new()));
    registry.register(Arc::new(opencode::OpencodeAdapter::new()));
    registry.register(Arc::new(grok::GrokAdapter::new()));
    registry
}

/// True when `path` is absolute and executable by the current user.
#[must_use]
pub fn is_absolute_executable(path: &Path) -> bool {
    if !path.is_absolute() {
        return false;
    }
    let Ok(meta) = path.metadata() else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn harness_crate_does_not_depend_on_a_pty() {
        // Structural: the harness crate's own manifest must not name a
        // pseudo-terminal package. Auxiliary terminals live in the server.
        let manifest = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"));
        let forbidden = ["portable-pty", "ptyprocess", "pty-process", "conpty"];
        for needle in forbidden {
            assert!(
                !manifest.contains(needle),
                "harness crate must not depend on a PTY library ({needle})"
            );
        }
        let lock = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../Cargo.lock"));
        let package = lock
            .split("[[package]]")
            .find(|block| block.contains("name = \"tidebreak-harness\""))
            .expect("tidebreak-harness is in Cargo.lock");
        for needle in forbidden {
            assert!(
                !package.contains(needle),
                "tidebreak-harness lock entry names a PTY library ({needle})"
            );
        }
    }

    #[test]
    fn filter_child_env_strips_tidebreak_keys() {
        let snapshot = vec![
            (
                std::ffi::OsString::from("TIDEBREAK_TEST_SECRET"),
                std::ffi::OsString::from("nope"),
            ),
            (
                std::ffi::OsString::from("GATEWAY_URL"),
                std::ffi::OsString::from("keep"),
            ),
        ];
        let env = filter_child_env(snapshot);
        assert!(env.iter().all(|(key, _)| {
            !key.to_string_lossy()
                .to_ascii_uppercase()
                .starts_with("TIDEBREAK_")
        }));
        assert!(env
            .iter()
            .any(|(key, value)| { key == "GATEWAY_URL" && value == "keep" }));
    }
}
