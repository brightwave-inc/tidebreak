//! Protocol translation from an external agent engine into one normalized
//! event vocabulary.
//!
//! Nothing in this crate's traits assumes the engine is a coding agent.
//! Tidebreak's own internal loop is a future implementor of the same
//! contract. Orchestration, persistence, and UI consume only
//! [`tidebreak_core::Event`]; this crate emits the unpersisted sibling
//! [`HarnessEvent`].

#![deny(missing_docs)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tidebreak_core::{
    ApprovalKind, BoundedError, Diffstat, FileChangeKind, GrantScope, HarnessCaps, HarnessCommand,
    HarnessKind, HarnessNoticeLevel, OwnerId, PermissionMode, ReasoningEffort, SessionId,
    ToolDetail, ToolOutcome, TurnId, TurnUsage, UserQuestionAnswer,
};

pub mod browser_channel;
pub mod budget;
pub mod child;
pub mod claude;
pub mod codex;
pub mod grok;
pub mod launch;
pub mod opencode;
pub mod pin;
pub mod probe;
mod text;
pub mod wiring;

pub use budget::{BudgetTick, StreamBudget, StreamLineBuffer};
pub use child::{
    current_process_identity, spawn_process_tree, spawned_process_identity,
    terminate_recorded_process, BoundedOutput, BoundedProcessOutput, ChildPid, OutputBudget,
    OutputRetention, ProcessTreeChild, RecordedProcessReap,
};
pub use launch::{
    validate_launch_plan, validate_launch_plan_with, BypassFlagError, BypassPolicy, LaunchPlan,
};
pub use pin::{
    compare_versions, ensure_installed, ensure_installed_version, installed_versions,
    latest_published_version, managed_binary, managed_binary_version, pin_for, HarnessPin, PINS,
};
pub use probe::{
    capture_login_env, display_model_label, env_value, filter_child_env, filter_engine_child_env,
    infer_listed_default, list_cli_models, observe_version, prefer_gateway_models, probe_shell,
    resolve_binary, resolve_command_on_path, with_reasoning_efforts, DeclaredBinary, HostEnv,
    ListedHarnessModel, ProbeCapture, ProbeError,
};

/// Whether some auth mode besides the vendor login a probe observes could
/// carry this engine's inference: an API key or endpoint override in the
/// captured environment, or engine config pointing inference at a gateway.
///
/// `true` grants the benefit of the doubt — a session may work even though
/// the probe saw signed-out — so callers use it to avoid refusing that
/// session, never as proof of working credentials. Engines whose override
/// surfaces are unverified answer `true` for the same reason.
#[must_use]
pub fn auth_override_present(
    kind: HarnessKind,
    env: &[(std::ffi::OsString, std::ffi::OsString)],
) -> bool {
    match kind {
        HarnessKind::ClaudeCode => claude::auth_override_present(env),
        HarnessKind::Codex => codex::auth_override_present(env),
        HarnessKind::Opencode | HarnessKind::Grok => true,
        // The in-process engine takes inference from the server's own
        // provider resolution; there is no engine-side credential to
        // override.
        HarnessKind::Internal => false,
    }
}

/// Which surface carried the credential override Tidebreak observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthOverrideSignal {
    /// An API key, auth token, or endpoint override in the captured shell
    /// environment.
    Environment,
    /// The engine's own configuration file points inference at an endpoint
    /// the vendor login does not cover.
    EngineConfig,
}

/// What Tidebreak observed about how one engine authenticates on this
/// machine, beyond the vendor login the probe reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HarnessAuthObservation {
    /// Nothing observed: the surfaces Tidebreak reads carry no override, or
    /// it reads none for this engine.
    Unknown,
    /// A credential or endpoint override is present, carried by this signal.
    Override(AuthOverrideSignal),
}

impl HarnessAuthObservation {
    /// Whether an override is present. `false` means unobserved, not absent.
    #[must_use]
    pub fn is_override(self) -> bool {
        matches!(self, Self::Override(_))
    }

    /// The signal that carried the override, when one is present.
    #[must_use]
    pub fn signal(self) -> Option<AuthOverrideSignal> {
        match self {
            Self::Unknown => None,
            Self::Override(signal) => Some(signal),
        }
    }
}

/// Observe how this engine authenticates here, for a caller that displays
/// the answer rather than one that decides whether to refuse a session.
///
/// Unlike [`auth_override_present`], this never grants the benefit of the
/// doubt: an engine whose override surfaces Tidebreak does not read answers
/// [`HarnessAuthObservation::Unknown`], because claiming a machine is
/// gateway-managed on no evidence would tell the reader their engine works
/// when nothing says it does. Claude Code and Codex read the same
/// environment and engine config the create path reads; opencode and Grok
/// answer `Unknown`.
#[must_use]
pub fn observe_auth_mode(
    kind: HarnessKind,
    env: &[(std::ffi::OsString, std::ffi::OsString)],
) -> HarnessAuthObservation {
    let signal = match kind {
        HarnessKind::ClaudeCode => claude::observe_auth_override(env),
        HarnessKind::Codex => codex::observe_auth_override(env),
        HarnessKind::Opencode | HarnessKind::Grok | HarnessKind::Internal => None,
    };
    signal.map_or(
        HarnessAuthObservation::Unknown,
        HarnessAuthObservation::Override,
    )
}

/// Normalized, unpersisted event. Maps 1:1 onto [`tidebreak_core::Event`]
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
        /// The `Task` call this message ran inside, when the engine tagged it
        /// as a subagent's (decision 52). Absent on the parent's own messages.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_call_id: Option<String>,
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
        /// The `Task` call this call ran inside, when the engine tagged it
        /// as a subagent's (decision 52). Absent on the parent's own calls.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_call_id: Option<String>,
    },
    /// A tool call finished.
    ToolCompleted {
        /// Engine-native call id.
        call_id: String,
        /// How it finished.
        outcome: ToolOutcome,
        /// Bounded preview.
        preview: String,
        /// Classification rebuilt from the call's complete arguments, when
        /// the engine reports them by the time the call resolves. Engines
        /// open a call before its arguments finish streaming, so the detail
        /// on [`HarnessEvent::ToolStarted`] can name nothing; this corrects
        /// it. `None` when the adapter never sees final arguments.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<ToolDetail>,
        /// The `Task` call this call ran inside, when the engine tagged it
        /// as a subagent's (decision 52). Absent on the parent's own calls.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_call_id: Option<String>,
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
        /// Exact structured classification, when the engine can state one.
        ///
        /// An engine that classifies its own request precisely — the internal
        /// engine's tool, questions, and plan approvals — ships it here so
        /// the server persists it instead of guessing from `raw`. External
        /// adapters leave it `None` and the server keeps its best-effort
        /// classification.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<ApprovalKind>,
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
        usage: TurnUsage,
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

/// Server-issued binding for one exact parked approval.
///
/// Claude's permission bridge receives engine-controlled call IDs over MCP.
/// The server adds this binding before it persists the approval, so a repeated
/// call ID cannot redirect a later decision to another session or request.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct HarnessApprovalCapability {
    /// Opaque server-issued token. The server uses the durable approval ID.
    pub token: String,
    /// Durable owner whose session requested the approval.
    pub owner_id: String,
    /// Durable approval row this capability resolves.
    pub approval_id: String,
    /// Session the permission request arrived on.
    pub session_id: String,
    /// Turn that was running when the permission request arrived.
    pub turn_id: String,
    /// Worker epoch that owned the native request.
    pub spawn_epoch: i64,
    /// SHA-256 of the exact permission request.
    pub request_sha256: String,
}

/// Engine-native handle for a parked approval.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct HarnessApprovalRef {
    /// Call or request id the engine will recognize on decide.
    pub call_id: String,
    /// Exact server binding for bridges whose engine IDs are not unique.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability: Option<HarnessApprovalCapability>,
}

impl HarnessApprovalRef {
    /// Create an engine-native reference before a server binding exists.
    #[must_use]
    pub fn engine(call_id: impl Into<String>) -> Self {
        Self {
            call_id: call_id.into(),
            capability: None,
        }
    }
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
    /// Approve, and mint a standing grant at this scope.
    ///
    /// Accepted only by engines declaring `standing_grants` in their
    /// capability vector; every other adapter rejects it with
    /// [`HarnessError::DecisionUnsupported`]. The route layer never offers
    /// the rung for an engine that does not declare the capability, so the
    /// rejection is a backstop, not a UI path (decision 0033 / 0048).
    ApproveWithGrant {
        /// The scope the decider granted.
        scope: GrantScope,
    },
    /// Answer a structured questions approval.
    ///
    /// Accepted only by engines declaring `user_questions`.
    Answers {
        /// The supplied answers, already validated by the caller.
        answers: Vec<UserQuestionAnswer>,
    },
    /// Decide a plan approval.
    ///
    /// Accepted only by engines declaring `plan_mode` with a plan-approval
    /// channel. On acceptance the caller follows with
    /// [`HarnessSession::set_permission_mode`] per the engine's declared
    /// lifecycle; this decision itself does not change the posture.
    PlanDecision {
        /// Whether the plan was accepted.
        approve: bool,
        /// Feedback returned to the engine, when any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        feedback: Option<String>,
    },
}

impl From<ApprovalDecision> for tidebreak_core::ApprovalDecisionKind {
    fn from(decision: ApprovalDecision) -> Self {
        match decision {
            ApprovalDecision::Approve => Self::Approve,
            ApprovalDecision::Deny { feedback } => Self::Deny { feedback },
            ApprovalDecision::ApproveWithGrant { scope } => Self::ApprovedWithGrant { scope },
            ApprovalDecision::Answers { answers } => Self::Answered { answers },
            ApprovalDecision::PlanDecision { approve, feedback } => {
                Self::PlanDecided { approve, feedback }
            }
        }
    }
}

/// One user turn to feed the engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnInput {
    /// The host-named turn this engine call drives. The internal engine
    /// uses it; every other adapter ignores it.
    pub turn_id: Option<TurnId>,
    /// The user's message.
    pub text: String,
    /// Model for this turn, when the engine takes one per child.
    pub model: Option<String>,
    /// Reasoning effort for this turn. `None` leaves the engine's own default
    /// alone. The server validates this against the selected adapter and model
    /// before the turn starts. The adapter maps the level onto whatever its
    /// engine spells and may still degrade it if the external catalog changes
    /// between validation and launch.
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Whether this turn runs in the engine's fast mode.
    ///
    /// The server sets this only when the selected model advertises the tier.
    /// Adapters still omit an unsupported tier defensively if an external
    /// catalog changes between validation and launch.
    pub fast_mode: bool,
    /// Images already published to the blob store and hydrated for this turn.
    pub images: Vec<TurnImage>,
}

/// One image on a turn's machine-readable input. Debug prints size, not pixels.
#[derive(Clone, PartialEq, Eq)]
pub struct TurnImage {
    /// IANA media type, already sniffed at publish (`image/png`, …).
    pub media_type: String,
    /// Pixel bytes. Never journaled.
    pub bytes: Vec<u8>,
}

impl std::fmt::Debug for TurnImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TurnImage")
            .field("media_type", &self.media_type)
            .field("byte_len", &self.bytes.len())
            .finish()
    }
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
    /// The engine durably checkpointed the turn and released it; the turn
    /// resumes through [`HarnessSession::resume_turn`] once the awaited
    /// dependency resolves.
    ///
    /// Only engines declaring `durable_parks` may return this. The turn is
    /// neither finished nor failed: no terminal turn event was emitted, and
    /// the caller persists `park_ref` so a restarted worker can resume
    /// against durable state rather than a live process.
    Parked {
        /// Engine-owned opaque token naming the checkpoint. The caller
        /// stores it verbatim and hands it back on resume.
        park_ref: String,
        /// What the park waits on, so the caller knows when to resume.
        waiting_on: ParkWait,
    },
}

/// The dependency a parked turn waits on.
///
/// Ids are the durable server-side identifiers (approval rows, tool call
/// ids, agent run ids) rendered as strings, so the contract stays neutral
/// about which id space an engine's host uses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ParkWait {
    /// A pending approval: a tool approval, a questions card, or a plan
    /// proposal. Named by the engine-native call id, because the engine
    /// never learns the server's durable row id; the worker resolves it to
    /// the approval row it recorded for that call.
    Approval {
        /// Engine-native call id, as on [`HarnessApprovalRef`].
        call_id: String,
    },
    /// A tool call executed outside the engine, by a client the server
    /// leases.
    ClientToolCall {
        /// Durable tool call id.
        call_id: String,
    },
    /// A set of background agent runs; the turn resumes when all settle.
    AgentRuns {
        /// Durable agent run ids.
        run_ids: Vec<String>,
    },
}

/// What resolved a parked turn's wait, handed to
/// [`HarnessSession::resume_turn`].
///
/// Each variant is a notification, not a payload: the resolution itself —
/// the decision, the tool result, the run outcomes — is durable before
/// resume is called, and the engine reads it from its own state. Passing
/// bodies here would make the resume message a second copy of durable
/// truth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumeInput {
    /// The awaited approval was decided, with this decision.
    ApprovalDecided {
        /// Engine-native call id of the approval that settled.
        call_id: String,
        /// The decision, already durably recorded.
        decision: ApprovalDecision,
    },
    /// The awaited client tool call completed and its result is durable.
    ClientToolCompleted {
        /// The call that settled.
        call_id: String,
    },
    /// Every awaited agent run settled.
    AgentRunsSettled {
        /// The runs that settled.
        run_ids: Vec<String>,
    },
}

/// Completes a parked engine approval. Implemented by the server bridge so
/// [`HarnessSession::decide`] can run while `run_turn` is blocked on the child.
#[async_trait]
pub trait ApprovalCompleter: Send + Sync {
    /// Finish the exact native channel this reference identifies.
    async fn complete(
        &self,
        approval: &HarnessApprovalRef,
        decision: ApprovalDecision,
    ) -> Result<(), HarnessError>;
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

/// Session-private browser capability-file path and trusted bridge
/// executable.
///
/// The trusted browser runtime writes a short-lived JSON capability file
/// at `capability_file` before the engine child is spawned. The adapter
/// injects only that path through a harness-owned environment key; no
/// token, URL, or other secret enters argv, logs, approval previews, or
/// persisted session metadata. The capability file is session-scoped: the
/// engine's tool bridge reads it once at startup and the runtime revokes
/// it when the session ends.
///
/// `bridge_command` is the absolute path to the trusted CLI executable the
/// engine's MCP server or CLI fallback invokes to reach the browser
/// channel. The server validates that it is absolute; the desktop sibling
/// resolver owns existence, file-type, and executable checks. This path
/// is safe to serialize into engine config because it carries no secret —
/// the capability file is the only credential, and it travels through the
/// environment, never argv.
///
/// Both halves are required: a `BrowserChannelSpec` cannot exist with a
/// capability file but no bridge command, or vice versa. The server mints
/// one only when both the native [`BrowserRuntime`] and the bridge
/// executable are present (issue #2372).
///
/// This struct carries only the metadata adapters need to construct the
/// launch environment. The mint, write, and revoke lifecycle is owned by
/// the trusted browser runtime (issue #2342).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct BrowserChannelSpec {
    /// Absolute path to the session-private capability file.
    pub capability_file: std::path::PathBuf,
    /// Absolute path to the trusted bridge executable (the `tidebreak` CLI
    /// sidecar). The server validates absoluteness; the desktop sibling
    /// resolver validates existence and executability.
    pub bridge_command: std::path::PathBuf,
    /// Whether the native runtime can synthesize trusted semantic actions.
    ///
    /// Adapters use this only to advertise an action verb. The browser
    /// runtime still authorizes every action at dispatch time.
    pub semantic_actions: bool,
}

impl BrowserChannelSpec {
    /// Adapter-owned environment key injected into every engine child.
    ///
    /// Adapters reject `TIDEBREAK_` keys from settings with
    /// [`Self::is_reserved_env_key`], while [`filter_child_env`] narrows the
    /// shell snapshot to an allowlist that excludes the same namespace.
    /// [`Self::inject_env_tokio`]
    /// then runs after `plan.env`, making this the final child value.
    pub const ENV_KEY: &'static str = "TIDEBREAK_BROWSER_CAPFILE";

    /// Construct a new spec from both required halves.
    ///
    /// The server is the only constructor: it validates that
    /// `bridge_command` is absolute before calling this. The desktop
    /// sibling resolver separately checks existence and executability.
    #[must_use]
    pub fn new(capability_file: std::path::PathBuf, bridge_command: std::path::PathBuf) -> Self {
        Self {
            capability_file,
            bridge_command,
            semantic_actions: false,
        }
    }

    /// Advertise semantic actions when the native runtime supports them.
    #[must_use]
    pub fn with_semantic_actions(mut self, semantic_actions: bool) -> Self {
        self.semantic_actions = semantic_actions;
        self
    }

    /// Return the trusted bridge executable path.
    #[must_use]
    pub fn bridge_command(&self) -> &std::path::Path {
        &self.bridge_command
    }

    /// Return the exact key/value pair adapters inject into engine children.
    #[must_use]
    pub fn env_pair(&self) -> (&'static str, &std::ffi::OsStr) {
        (Self::ENV_KEY, self.capability_file.as_os_str())
    }

    /// Whether a settings environment key belongs to Tidebreak rather than
    /// the user. Adapters must reject these keys before composing a launch.
    ///
    /// The whole namespace is reserved. The session relay key
    /// ([`crate::wiring`], decision 71) is carried under a caller-chosen
    /// name in [`crate::SessionSpec::relay_key_env`]; adapters that must let
    /// it through use [`Self::is_reserved_env_key_except`] with exactly that
    /// name.
    #[must_use]
    pub fn is_reserved_env_key(key: &str) -> bool {
        key.to_ascii_uppercase().starts_with("TIDEBREAK_")
    }

    /// [`Self::is_reserved_env_key`] with one exact exemption: the relay key
    /// variable the caller wired ([`crate::SessionSpec::relay_key_env`]).
    ///
    /// The exemption matches case-sensitively — the wiring writes one exact
    /// name, and a differently-cased look-alike from settings stays
    /// reserved.
    #[must_use]
    pub fn is_reserved_env_key_except(key: &str, relay_key_env: Option<&str>) -> bool {
        if relay_key_env.is_some_and(|allowed| key == allowed) {
            return false;
        }
        Self::is_reserved_env_key(key)
    }

    /// Inject the capability-file path as the final environment value on a
    /// Tokio engine command.
    pub fn inject_env_tokio(&self, cmd: &mut tokio::process::Command) {
        let (key, value) = self.env_pair();
        cmd.env(key, value);
    }
}

/// What an adapter needs to spawn or connect one session.
pub struct SessionSpec {
    /// Principal the session acts for. An in-process engine resolves its
    /// inference and scopes its durable state by this; external engines
    /// carry their own credentials and ignore it.
    pub owner: OwnerId,
    /// The durable session this launch serves. An in-process engine keys
    /// its own durable state on it, so a relaunch finds the same state
    /// without a native resume token; external engines ignore it.
    pub session_id: SessionId,
    /// Worktree the engine should use as its working directory.
    pub worktree: PathBuf,
    /// Absolute directory roots that engine tools may read outside the worktree.
    pub allowed_read_roots: Vec<PathBuf>,
    /// Permission mode. Adapters refuse a mode they cannot honor.
    pub permission_mode: PermissionMode,
    /// Engine model id, when the session chose one.
    pub model: Option<String>,
    /// Reasoning effort the session starts on, when it chose one.
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Whether the session starts in the engine's fast mode.
    pub fast_mode: bool,
    /// Engine-native resume token, when continuing a prior session.
    pub resume_ref: Option<String>,
    /// Extra argv from settings. Still subject to the bypass-flag denylist.
    pub extra_argv: Vec<String>,
    /// Extra environment from settings. Cannot override adapter-owned keys.
    pub extra_env: Vec<(String, String)>,
    /// Environment variable name in `extra_env` that carries the session
    /// inference relay key, when the caller wired one ([`crate::wiring`],
    /// decision 71). Adapters let exactly this key survive the
    /// reserved-namespace strip — or, for an engine that reads credentials
    /// from a file, consume the value under this name instead of passing it
    /// through. `None` means no relay is wired and the whole reserved
    /// namespace is stripped.
    pub relay_key_env: Option<String>,
    /// Shell-resolved environment captured by the probe. Children run under
    /// the [`filter_child_env`] allowlist subset of this snapshot, not the
    /// GUI process environment — ambient shell-rc credentials never reach an
    /// engine child. A credential a session needs travels through
    /// [`Self::extra_env`] or the relay instead.
    pub env: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    /// Approval-channel wiring, when the server has one to offer.
    pub approval: Option<ApprovalChannelSpec>,
    /// Absolute engine binary, already resolved by [`probe`], or `None` for
    /// an engine that runs in-process and spawns no child. Adapters that
    /// drive an external CLI refuse a spec without one
    /// ([`HarnessError::NotFound`]).
    pub binary: Option<PathBuf>,
    /// Where normalized events go.
    pub sink: Arc<dyn HarnessEventSink>,
    /// Browser channel wiring, when the trusted browser runtime has
    /// produced a session-private capability file. `None` preserves the
    /// existing behavior: no browser tools are advertised or injected.
    pub browser: Option<BrowserChannelSpec>,
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

    /// Every effort level this engine accepts, ascending, across all models.
    ///
    /// The ladder a *model* takes can be narrower — Codex states one per row —
    /// so callers use this only for the implicit engine default when the
    /// catalog does not identify one. Empty means the engine takes no effort
    /// control.
    fn reasoning_efforts(&self, probe: &HarnessProbe) -> Vec<ReasoningEffort> {
        let _ = probe;
        Vec::new()
    }

    /// Models this engine currently lists. Empty when the CLI has no catalog.
    async fn list_models(&self, probe: &HarnessProbe) -> Vec<ListedHarnessModel> {
        let Some(binary) = probe.binary_path.as_deref() else {
            return Vec::new();
        };
        prefer_gateway_models(list_cli_models(binary, &["models"], &probe.env).await)
    }

    /// Whether relaunching a session composes a permission mode that changed
    /// after it started.
    ///
    /// True for every engine that takes its posture from the launch plan: a
    /// relaunch rebuilds that plan from the stored mode, so the new one lands.
    /// False where the engine fixes its posture when the session is *created*
    /// and resuming an existing one does not re-apply it — there the relaunch
    /// is silent about the change, and the caller must refuse rather than let
    /// the record and the engine disagree.
    fn relaunch_composes_permission_mode(&self) -> bool {
        true
    }

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

    /// Continue a turn that ended as [`TurnOutcome::Parked`], once the wait
    /// it named has resolved.
    ///
    /// Events flow to the sink exactly as in [`Self::run_turn`], continuing
    /// after the checkpoint rather than replaying it. A resumed turn may
    /// park again. The default refuses: only an engine declaring
    /// `durable_parks` implements this, and it must accept every `park_ref`
    /// it has returned — including across a relaunch, because the park is
    /// durable and the process that minted it may be gone.
    async fn resume_turn(
        &self,
        park_ref: String,
        input: ResumeInput,
    ) -> Result<TurnOutcome, HarnessError> {
        let _ = (park_ref, input);
        Err(HarnessError::ParkResumeUnsupported)
    }

    /// Resolve a pending approval through the engine's native channel.
    async fn decide(
        &self,
        approval: HarnessApprovalRef,
        decision: ApprovalDecision,
    ) -> Result<(), HarnessError>;

    /// Ask the engine to stop the current turn.
    async fn interrupt(&self) -> Result<(), HarnessError>;

    /// Move a live session onto a new permission mode.
    ///
    /// The default refuses, and the caller relaunches the engine against the
    /// new mode. Override it when the engine has a channel that re-postures a
    /// running session — a control request it honors on an open child, or a
    /// per-turn policy field it reads on the next turn. Either way the new
    /// mode governs from the next turn on; this is not a way to re-posture the
    /// turn already in flight.
    ///
    /// An adapter that accepts the change owns remembering it: a later
    /// relaunch of the same session must compose the new mode, not the one the
    /// spec was built with.
    async fn set_permission_mode(&self, mode: PermissionMode) -> Result<(), HarnessError> {
        let _ = mode;
        Err(HarnessError::PermissionModeSwitchUnsupported)
    }

    /// Inject a mid-turn user message, when the engine accepts one.
    ///
    /// The default refuses: an adapter must override this only when its
    /// capability vector states [`CapLevel::Supported`] for mid-turn steering.
    /// An accepting adapter owns the matching [`HarnessEvent::UserSteered`]
    /// emission: emit it exactly once before returning `Ok(())`, and emit none
    /// when admission is rejected. Keeping acknowledgement and event ordering
    /// inside the protocol adapter lets a terminal event that follows in the
    /// same native batch remain causally after the user's guidance.
    async fn steer(&self, text: String) -> Result<(), HarnessError> {
        let _ = text;
        Err(HarnessError::SteeringUnsupported)
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

    /// Release the engine's processes while the session stays attachable, so
    /// an idle session stops holding a runtime resident (decision 0064).
    ///
    /// Called only between turns. A later [`Self::run_turn`] must
    /// transparently restore whatever this released — respawn and resume from
    /// the session's ref — so a park is invisible apart from the wake turn's
    /// spawn latency. Idempotent: parking a session with nothing running is a
    /// no-op. The default does nothing, for engines with no between-turn
    /// child to release.
    async fn park(&self) -> Result<(), HarnessError> {
        Ok(())
    }

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
    /// Engine-owned slash commands, empty when the adapter has no listing.
    pub commands: Vec<HarnessCommand>,
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
    /// An allowed read root was not absolute.
    #[error("allowed read root must be absolute: {0}")]
    AllowedReadRootNotAbsolute(String),
    /// The engine cannot honor the requested permission mode.
    #[error("permission mode {0} is not available on this engine")]
    PermissionModeUnsupported(PermissionMode),
    /// The engine no longer knows the session this spec asked to resume.
    ///
    /// The stored resume ref is dead: retrying with it fails identically
    /// forever, so the caller must drop it and start a fresh engine session
    /// rather than treat this as one failed turn.
    #[error("the engine no longer has this session: {0}")]
    ResumeLost(String),
    /// The engine cannot change permission mode on a live session.
    ///
    /// Distinct from [`Self::PermissionModeUnsupported`]: the mode itself is
    /// fine, the engine just fixes its posture at launch. The caller relaunches
    /// against the new mode instead of giving up.
    #[error("this engine sets its permission mode at launch")]
    PermissionModeSwitchUnsupported,
    /// The engine exposes a live switch, but did not confirm this request.
    ///
    /// The adapter keeps the prior mode and retires an ambiguous child before
    /// returning this error, so a later turn cannot run under an unconfirmed
    /// posture.
    #[error("the engine did not confirm the permission mode change: {0}")]
    PermissionModeSwitchFailed(String),
    /// A Plan-mode engine changed its default plan storage outside the
    /// worktree after Tidebreak redirected plan files to private storage.
    #[error("plan mode wrote outside the worktree: {0}")]
    PlanWriteOutsideWorktree(String),
    /// The adapter has no verified same-turn steering channel.
    #[error("mid-turn steering is not available on this engine")]
    SteeringUnsupported,
    /// The engine cannot park a turn durably or resume one.
    ///
    /// The default for every engine whose capability vector declares
    /// `durable_parks` as anything but supported.
    #[error("durable turn parks are not available on this engine")]
    ParkResumeUnsupported,
    /// The engine's native channel cannot express this approval decision.
    ///
    /// A backstop: the route layer only offers decisions the engine's
    /// capability vector declares, so reaching this means a caller skipped
    /// the gate.
    #[error("this engine cannot take that approval decision: {0}")]
    DecisionUnsupported(String),
    /// The native engine refused a steer for the currently active turn.
    #[error("the engine refused mid-turn steering: {0}")]
    SteeringRejected(String),
    /// The server can no longer prove that a native approval waiter exists.
    #[error("the approval request is no longer waiting: {0}")]
    ApprovalWaiterMissing(String),
    /// The waiter received the decision, but its acknowledgement was lost.
    #[error("the approval decision may have been delivered: {0}")]
    ApprovalAcknowledgementLost(String),
    /// The server-issued approval binding does not match the parked request.
    #[error("the approval request binding does not match: {0}")]
    ApprovalBindingMismatch(String),
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

    /// The decision and park-wait shapes ride `HarnessEvent` payloads and the
    /// scripted-harness script variable, so their tags are a contract.
    #[test]
    fn decisions_and_park_waits_round_trip() {
        let decisions = [
            ApprovalDecision::ApproveWithGrant {
                scope: GrantScope::WholeTool,
            },
            ApprovalDecision::Answers {
                answers: vec![UserQuestionAnswer {
                    question_id: "q1".into(),
                    selected_option_ids: vec!["a".into()],
                    custom_answer: None,
                }],
            },
            ApprovalDecision::PlanDecision {
                approve: false,
                feedback: Some("tighten the scope".into()),
            },
        ];
        let tags = ["approve_with_grant", "answers", "plan_decision"];
        for (decision, tag) in decisions.iter().zip(tags) {
            let json = serde_json::to_value(decision).unwrap();
            assert_eq!(json["type"], tag);
            let back: ApprovalDecision = serde_json::from_value(json).unwrap();
            assert_eq!(&back, decision);
        }
        let wait = ParkWait::AgentRuns {
            run_ids: vec!["run-1".into(), "run-2".into()],
        };
        let json = serde_json::to_value(&wait).unwrap();
        assert_eq!(json["type"], "agent_runs");
        assert_eq!(serde_json::from_value::<ParkWait>(json).unwrap(), wait);
    }

    /// Every decision maps onto exactly one journal resolution kind.
    #[test]
    fn every_decision_has_a_journal_resolution() {
        use tidebreak_core::ApprovalDecisionKind as Kind;
        assert_eq!(Kind::from(ApprovalDecision::Approve), Kind::Approve);
        assert_eq!(
            Kind::from(ApprovalDecision::Deny { feedback: None }),
            Kind::Deny { feedback: None }
        );
        assert_eq!(
            Kind::from(ApprovalDecision::PlanDecision {
                approve: true,
                feedback: None
            }),
            Kind::PlanDecided {
                approve: true,
                feedback: None
            }
        );
    }

    #[test]
    fn claude_codex_and_grok_recompose_permission_mode_on_relaunch() {
        assert!(claude::ClaudeCodeAdapter::new().relaunch_composes_permission_mode());
        assert!(codex::CodexAdapter::new().relaunch_composes_permission_mode());
        assert!(grok::GrokAdapter::new().relaunch_composes_permission_mode());
        assert!(!opencode::OpencodeAdapter::new().relaunch_composes_permission_mode());
    }

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
                std::ffi::OsString::from("ambient"),
            ),
            (
                std::ffi::OsString::from("HOME"),
                std::ffi::OsString::from("/home/probe"),
            ),
        ];
        let env = filter_child_env(snapshot);
        assert!(env.iter().all(|(key, _)| {
            !key.to_string_lossy()
                .to_ascii_uppercase()
                .starts_with("TIDEBREAK_")
        }));
        // The child filter is an allowlist: an arbitrary ambient variable is
        // dropped alongside the reserved namespace, not just renamed around.
        assert!(env.iter().all(|(key, _)| key != "GATEWAY_URL"));
        assert!(env
            .iter()
            .any(|(key, value)| { key == "HOME" && value == "/home/probe" }));
    }

    /// A signing-key passphrase prompt in a headless child never returns, so
    /// the agent socket must reach the child or every signed `git commit`
    /// hangs until the tool times out.
    #[test]
    fn filter_child_env_keeps_the_ssh_agent_socket() {
        let snapshot = vec![(
            std::ffi::OsString::from("SSH_AUTH_SOCK"),
            std::ffi::OsString::from("/run/user/1000/agent.sock"),
        )];
        let env = filter_child_env(snapshot);
        assert!(env.iter().any(|(key, value)| {
            key == "SSH_AUTH_SOCK" && value == "/run/user/1000/agent.sock"
        }));
    }

    #[test]
    fn no_adapter_declares_image_input_without_an_image_fixture() {
        use tidebreak_core::CapLevel;

        let probe = HarnessProbe {
            found: true,
            binary_path: None,
            version: Some("test".into()),
            authenticated: None,
            stderr: String::new(),
            env: Vec::new(),
            commands: Vec::new(),
        };
        let cases: [(&str, Box<dyn HarnessAdapter>, &str); 4] = [
            (
                "claude_code",
                Box::new(claude::ClaudeCodeAdapter::new()),
                "claude-code/2.1.233",
            ),
            (
                "codex",
                Box::new(codex::CodexAdapter::new()),
                "codex/0.147.0",
            ),
            (
                "opencode",
                Box::new(opencode::OpencodeAdapter::new()),
                "opencode/1.18.18",
            ),
            ("grok", Box::new(grok::GrokAdapter::new()), "grok/1.0.4"),
        ];
        for (kind, adapter, fixture_rel) in cases {
            let caps = adapter.capabilities(&probe);
            if caps.image_input != CapLevel::Supported {
                continue;
            }
            let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("fixtures")
                .join(fixture_rel);
            assert!(
                fixture_dir_has_image_roundtrip(&dir),
                "{kind} declares image_input Supported without a fixture directory containing an image round-trip capture"
            );
        }
    }

    fn fixture_dir_has_image_roundtrip(dir: &Path) -> bool {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return false;
        };
        entries.filter_map(Result::ok).any(|entry| {
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            name.contains("image") && (name.ends_with(".ndjson") || name.ends_with(".json"))
        })
    }

    /// A supervising user reads a tool call while it runs, so every adapter
    /// has to name the call when it starts.
    ///
    /// Engines open a call before its arguments finish streaming. An adapter
    /// that starts the call at that first view publishes an empty `cmd` or
    /// `path` and names the tool only once the result lands, which leaves an
    /// unlabelled card on screen for as long as the tool runs. Codex and
    /// grok already named their calls at the start; this pins that, and the
    /// claude-code and opencode captures that used to fail it.
    #[test]
    fn every_captured_tool_call_names_its_subject_when_it_starts() {
        for (harness, path) in captured_streams() {
            let input = std::fs::read_to_string(&path)
                .unwrap_or_else(|err| panic!("unreadable fixture {}: {err}", path.display()));
            let events = replay_captured_stream(&harness, &input);
            let fixture = path.display();
            let mut started: Vec<&str> = Vec::new();
            for event in &events {
                match event {
                    HarnessEvent::ToolStarted {
                        call_id,
                        name,
                        detail,
                        ..
                    } => {
                        assert!(
                            detail.specificity() > 0,
                            "{fixture}: {name} starts with a detail that names nothing"
                        );
                        started.push(call_id);
                    }
                    HarnessEvent::ToolCompleted { call_id, .. } => assert!(
                        started.contains(&call_id.as_str()),
                        "{fixture}: {call_id} completes without having started"
                    ),
                    _ => {}
                }
            }
        }
    }

    /// Every captured stream, as `(harness, path)` pairs.
    fn captured_streams() -> Vec<(String, PathBuf)> {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures");
        let mut streams = Vec::new();
        for harness in read_dir_sorted(&root) {
            let name = harness.file_name().to_string_lossy().into_owned();
            for version in read_dir_sorted(&harness.path()) {
                for capture in read_dir_sorted(&version.path()) {
                    let path = capture.path();
                    if path.extension().is_some_and(|ext| ext == "ndjson") {
                        streams.push((name.clone(), path));
                    }
                }
            }
        }
        assert!(!streams.is_empty(), "fixtures must ship captured streams");
        streams
    }

    fn read_dir_sorted(dir: &Path) -> Vec<std::fs::DirEntry> {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut entries: Vec<_> = entries.filter_map(Result::ok).collect();
        entries.sort_by_key(std::fs::DirEntry::file_name);
        entries
    }

    fn replay_captured_stream(harness: &str, input: &str) -> Vec<HarnessEvent> {
        match harness {
            "claude-code" => claude::parse::ClaudeStreamParser::parse_ndjson(input).events,
            "codex" => codex::parse::CodexStreamParser::parse_ndjson(input).events,
            "grok" => grok::parse::GrokStreamParser::parse_ndjson(input).events,
            "opencode" => opencode::parse::OpencodeStreamParser::parse_ndjson(input).events,
            other => panic!("fixtures/{other} has no parser in this test"),
        }
    }
}
