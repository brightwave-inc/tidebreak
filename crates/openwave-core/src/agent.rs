//! The agent loop: the turn engine that drives a conversation.
//!
//! One [`Agent`] ties together a [`ModelProvider`], a [`ToolRegistry`], and a
//! [`Store`], and runs a *turn* — one user input through to a final answer —
//! emitting [`AgentEvent`]s as it goes.
//!
//! Per turn the loop: assembles the request → streams the model call →, if the
//! model called tools, runs them and feeds the results back → repeats until the
//! model stops, bounded by a max-steps guard.
//!
//! v1 scope (deliberately small; each is a tracked follow-up):
//! - tool calls run **sequentially** (concurrency for independent calls later);
//! - approval is **auto** for `ReadOnly`/`Workspace`; `Sensitive` parks via an
//!   [`ApprovalGate`] until approve/reject unless a standing grant covers it;
//! - context reduction is deterministic floor+restore (no LLM summarization);
//!   retries with progressive reduction on provider prompt-too-long errors.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use futures::channel::{mpsc::UnboundedSender, oneshot};
use futures::future::{self, Either};
use futures::StreamExt;
use futures_timer::Delay;
use serde_json::Value;

use crate::agent_tools::{
    validate_spawn_sandbox_agent_arguments, validate_wait_for_agents_arguments,
    SpawnSandboxAgentArgs, WaitForAgentsArgs,
};
use crate::approval::{
    ApprovalDecision, ApprovalGate, ApprovalJournalIdentity, ApprovalRequest,
    ApprovalRequiredPublication, RefuseGate, StandingGrants, ToolApprovalKind,
};
use crate::cancel::CancelToken;
use crate::citation::{
    classify_source_reference_candidate, parse_assistant_citations, AssistantCitationReference,
    SourceReferenceCandidate,
};
use crate::context;
use crate::error::{AgentError, Result};
use crate::event::{AgentEvent, SequencedEvent};
use crate::id::{AgentRunId, CallId, ChatId, MessageId, TurnId};
use crate::model::{
    Chat, Message, Role, ToolCallExecution, ToolCallRecord, ToolCallResolution, ToolCallStatus,
    TurnRunStatus,
};
use crate::preview::{ToolActionPreview, ToolResultPreview};
use crate::provider::{
    ChatMessage, ChatRequest, ContentBlock, ModelProvider, ProviderEvent, StopReason, Usage,
};
use crate::steer::SteerInbox;
use crate::storage::{
    AcceptClaimedToolCallOutcome, AcceptToolCallOutcome, AppendClaimedMessageOutcome,
    ApplyTurnSteerOutcome, JournaledTurnSteerOutcome, ResolveToolCallOutcome, Store,
    TurnLeaseFence,
};
use crate::tool::{ApprovalClass, Tool, ToolCtx, ToolOutput, ToolScratch, ToolSpec};

/// A name-keyed registry of the tools available to the agent.
#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: HashMap<String, RegisteredTool>,
}

#[derive(Clone)]
enum RegisteredTool {
    Server(Arc<dyn Tool>),
    Client {
        spec: ToolSpec,
        validate_arguments: Option<fn(&Value) -> bool>,
    },
    ForegroundClient {
        spec: ToolSpec,
        validate_arguments: fn(&Value) -> bool,
    },
    ForegroundOrchestration {
        spec: ToolSpec,
        kind: ForegroundOrchestrationKind,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ForegroundOrchestrationKind {
    Spawn,
    Wait,
}

impl ToolRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a tool under its advertised name (replacing any existing one).
    pub fn register(&mut self, tool: Box<dyn Tool>) {
        self.tools
            .insert(tool.spec().name, RegisteredTool::Server(Arc::from(tool)));
    }

    /// Register a client-owned tool contract with no server-side executor.
    pub fn register_client(&mut self, spec: ToolSpec) {
        self.tools.insert(
            spec.name.clone(),
            RegisteredTool::Client {
                spec,
                validate_arguments: None,
            },
        );
    }

    /// Register a client-owned contract with payload validation at checkpoint time.
    pub fn register_validated_client(
        &mut self,
        spec: ToolSpec,
        validate_arguments: fn(&Value) -> bool,
    ) {
        self.tools.insert(
            spec.name.clone(),
            RegisteredTool::Client {
                spec,
                validate_arguments: Some(validate_arguments),
            },
        );
    }

    /// Register a validated client continuation that is visible only to a
    /// claimed foreground coordinator, never to sandbox/direct agent surfaces.
    pub fn register_validated_foreground_client(
        &mut self,
        spec: ToolSpec,
        validate_arguments: fn(&Value) -> bool,
    ) {
        self.tools.insert(
            spec.name.clone(),
            RegisteredTool::ForegroundClient {
                spec,
                validate_arguments,
            },
        );
    }

    /// Register the closed foreground-only spawn and ordered-wait contracts.
    ///
    /// A claimed foreground worker must still opt in before either definition
    /// is advertised. Sandboxed workers never opt in, keeping delegation depth
    /// bounded at one.
    pub fn register_foreground_agent_orchestration(&mut self) {
        for (spec, kind) in [
            (
                crate::spawn_sandbox_agent_tool_spec(),
                ForegroundOrchestrationKind::Spawn,
            ),
            (
                crate::wait_for_agents_tool_spec(),
                ForegroundOrchestrationKind::Wait,
            ),
        ] {
            self.tools.insert(
                spec.name.clone(),
                RegisteredTool::ForegroundOrchestration { spec, kind },
            );
        }
    }

    /// Builder-style [`register`](Self::register).
    #[must_use]
    pub fn with(mut self, tool: Box<dyn Tool>) -> Self {
        self.register(tool);
        self
    }

    /// Look up a tool by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        match self.tools.get(name) {
            Some(RegisteredTool::Server(tool)) => Some(tool.as_ref()),
            Some(RegisteredTool::Client { .. })
            | Some(RegisteredTool::ForegroundClient { .. })
            | Some(RegisteredTool::ForegroundOrchestration { .. })
            | None => None,
        }
    }

    /// Resolve the trusted execution surface for a registered tool name.
    #[must_use]
    pub fn execution(&self, name: &str) -> Option<ToolCallExecution> {
        Some(match self.tools.get(name)? {
            RegisteredTool::Server(_) => ToolCallExecution::Server,
            RegisteredTool::Client { .. } | RegisteredTool::ForegroundClient { .. } => {
                ToolCallExecution::Client
            }
            RegisteredTool::ForegroundOrchestration { .. } => return None,
        })
    }

    /// The specs of every registered tool, to advertise to the model.
    #[must_use]
    pub fn specs(&self) -> Vec<ToolSpec> {
        self.specs_for_foreground(false)
    }

    /// The model-visible definitions for one execution surface.
    ///
    /// The foreground coordinator may opt into the sandbox control tool. All
    /// other contexts receive the ordinary server/client tool set only.
    #[must_use]
    pub fn specs_for_foreground(&self, allow_agent_orchestration: bool) -> Vec<ToolSpec> {
        self.tools
            .values()
            .filter_map(|tool| match tool {
                RegisteredTool::Server(tool) => Some(tool.spec()),
                RegisteredTool::Client { spec, .. } => Some(spec.clone()),
                RegisteredTool::ForegroundClient { spec, .. } if allow_agent_orchestration => {
                    Some(spec.clone())
                }
                RegisteredTool::ForegroundClient { .. } => None,
                RegisteredTool::ForegroundOrchestration { spec, .. }
                    if allow_agent_orchestration =>
                {
                    Some(spec.clone())
                }
                RegisteredTool::ForegroundOrchestration { .. } => None,
            })
            .collect()
    }

    /// Validate canonical arguments against a registered client-owned contract.
    #[must_use]
    pub fn client_arguments_are_valid(&self, name: &str, arguments: &Value) -> bool {
        match self.tools.get(name) {
            Some(RegisteredTool::Client {
                validate_arguments: Some(validate),
                ..
            }) => validate(arguments),
            Some(RegisteredTool::Client {
                validate_arguments: None,
                ..
            }) => true,
            Some(RegisteredTool::ForegroundClient {
                validate_arguments, ..
            }) => validate_arguments(arguments),
            Some(RegisteredTool::Server(_))
            | Some(RegisteredTool::ForegroundOrchestration { .. })
            | None => false,
        }
    }

    /// Whether `name` is a client continuation restricted to a claimed
    /// foreground coordinator.
    #[must_use]
    pub fn is_foreground_client(&self, name: &str) -> bool {
        matches!(
            self.tools.get(name),
            Some(RegisteredTool::ForegroundClient { .. })
        )
    }

    /// Whether `name` identifies the foreground-only sandbox control tool.
    #[must_use]
    pub fn is_foreground_sandbox_spawn(&self, name: &str) -> bool {
        matches!(
            self.tools.get(name),
            Some(RegisteredTool::ForegroundOrchestration {
                kind: ForegroundOrchestrationKind::Spawn,
                ..
            })
        )
    }

    /// Parse and validate one foreground sandbox task.
    #[must_use]
    pub fn sandbox_spawn_task(&self, name: &str, arguments: &Value) -> Option<String> {
        if !self.is_foreground_sandbox_spawn(name)
            || !validate_spawn_sandbox_agent_arguments(arguments)
        {
            return None;
        }
        serde_json::from_value::<SpawnSandboxAgentArgs>(arguments.clone())
            .ok()
            .map(|arguments| arguments.task)
    }

    /// Whether `name` identifies the foreground-only ordered wait tool.
    #[must_use]
    pub fn is_foreground_agent_wait(&self, name: &str) -> bool {
        matches!(
            self.tools.get(name),
            Some(RegisteredTool::ForegroundOrchestration {
                kind: ForegroundOrchestrationKind::Wait,
                ..
            })
        )
    }

    /// Parse and validate one ordered foreground child wait.
    #[must_use]
    pub fn wait_for_agent_ids(&self, name: &str, arguments: &Value) -> Option<Vec<AgentRunId>> {
        if !self.is_foreground_agent_wait(name) || !validate_wait_for_agents_arguments(arguments) {
            return None;
        }
        serde_json::from_value::<WaitForAgentsArgs>(arguments.clone())
            .ok()
            .map(|arguments| arguments.agent_ids)
    }

    /// Whether no tools are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

/// Default cap on a single tool result fed back to the model: 64 KiB (~16k
/// tokens), enough for typical files while bounding a runaway read. A rough
/// byte-proxy for a token budget; token-accurate capping + paging come later.
pub const DEFAULT_MAX_TOOL_RESULT_BYTES: usize = 64 * 1024;

/// Per-turn tuning for an [`Agent`].
#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// Explicit provider route resolved from the host model registry.
    pub provider: Option<crate::provider::ProviderId>,
    /// Provider model identifier (e.g. `claude-opus-4-8`).
    pub model: String,
    /// Whether this model uses the provider's reasoning request shape.
    pub reasoning_model: bool,
    /// Reasoning-effort hint for models that expose the control; ignored by the
    /// rest. `None` leaves the provider default in force.
    pub reasoning_effort: Option<crate::model::ReasoningEffort>,
    /// System prompt, if any.
    pub system_prompt: Option<String>,
    /// Upper bound on tokens to generate per model call.
    pub max_tokens: Option<u32>,
    /// Sampling temperature.
    pub temperature: Option<f32>,
    /// Max model calls in one turn before the turn fails (loop guard).
    pub max_steps: usize,
    /// Max bytes of a single tool result fed back to the model; larger results
    /// are truncated with a notice, so one big read can't blow the context.
    pub max_tool_result_bytes: usize,
    /// The model's context window in tokens. Used to compute the message budget
    /// for context reduction (default: 200 000).
    pub context_window: usize,
    /// Exact runtime-only private scratch directory for legacy built-in file
    /// tools. It is derived by the embedding server and never persisted in a
    /// project or conversation.
    pub tool_scratch: Option<ToolScratch>,
}

/// Default context window: 200k tokens (Claude Opus/Sonnet).
pub const DEFAULT_CONTEXT_WINDOW: usize = 200_000;

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            provider: None,
            model: String::new(),
            reasoning_model: false,
            reasoning_effort: None,
            system_prompt: None,
            max_tokens: None,
            temperature: None,
            max_steps: 16,
            max_tool_result_bytes: DEFAULT_MAX_TOOL_RESULT_BYTES,
            context_window: DEFAULT_CONTEXT_WINDOW,
            tool_scratch: None,
        }
    }
}

/// The cooperative result of executing one durably claimed turn.
///
/// A completed output is returned to the worker instead of being persisted by
/// the agent loop. The worker can then commit the message and terminal turn
/// transition together through [`Store::complete_turn_run`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentTurnOutcome {
    /// The final assistant message prepared for atomic completion.
    Completed {
        /// The final message to publish with the terminal state transition.
        output: Message,
        /// Ordered opaque evidence references stripped from the final text.
        citations: Vec<AssistantCitationReference>,
        /// Aggregate provider usage for the eventual terminal event.
        usage: Usage,
        /// Provider stop reason for the eventual terminal event.
        stop_reason: StopReason,
        /// Durable steering epoch captured immediately before the final model call.
        steer_revision: Option<i64>,
        /// Model-call steps consumed from the turn-wide execution budget.
        model_steps: usize,
    },
    /// The loop observed its cancellation token and stopped cooperatively.
    Cancelled {
        /// Aggregate provider usage for the eventual terminal event.
        usage: Usage,
        /// Model-call steps consumed from the turn-wide execution budget.
        model_steps: usize,
    },
    /// The model requested one tool that must execute on a trusted client.
    ClientToolCall {
        /// Immutable call identity and canonical arguments to checkpoint.
        request: crate::model::ClientToolCallRequest,
        /// Provider usage incurred in this agent invocation.
        usage: Usage,
        /// Durable steering epoch captured before the producing model call.
        steer_revision: i64,
        /// Model-call steps consumed in this agent invocation.
        model_steps: usize,
    },
    /// The foreground model requested one durable sandbox child.
    ///
    /// The foreground worker validates this exact request and invokes
    /// [`Store::checkpoint_sandbox_spawn`] with its live lease, steering epoch,
    /// and accumulated checkpoint totals before yielding into `resuming`.
    SandboxAgentSpawn {
        /// Canonical child identity and bounded task derived from the tool call.
        request: SandboxAgentSpawnRequest,
        /// Provider usage incurred in this agent invocation.
        usage: Usage,
        /// Durable steering epoch captured before the producing model call.
        steer_revision: i64,
        /// Model-call steps consumed in this agent invocation.
        model_steps: usize,
    },
    /// The foreground model requested an ordered wait for sandbox children.
    WaitForAgents {
        /// Canonical wait identity and ordered child set.
        request: ForegroundAgentWaitRequest,
        /// Provider usage incurred in this agent invocation.
        usage: Usage,
        /// Durable steering epoch captured before the producing model call.
        steer_revision: i64,
        /// Model-call steps consumed in this agent invocation.
        model_steps: usize,
    },
    /// Execution failed after consuming provider work that must be retained.
    Failed {
        /// Stable terminal error payload for the durable failure event.
        error: crate::AgentErrorInfo,
        /// Aggregate provider usage consumed before the failure.
        usage: Usage,
        /// Model-call steps consumed before the failure.
        model_steps: usize,
    },
}

/// One model proposal to create a durable depth-one sandbox child.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxAgentSpawnRequest {
    /// Stable call identity emitted by the model stream.
    pub call_id: CallId,
    /// Provider-facing tool-use identity retained for transcript reconstruction.
    pub provider_id: String,
    /// Deterministic sandbox child identity derived from [`Self::call_id`].
    pub child_run_id: AgentRunId,
    /// Bounded, self-contained child input.
    pub task: String,
    /// Canonical closed arguments emitted by the provider.
    pub arguments: Value,
}

impl SandboxAgentSpawnRequest {
    /// Whether the immutable identities and task agree with the core contract.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        self.call_id.0 != uuid::Uuid::nil()
            && self.child_run_id == AgentRunId::sandbox_for_spawn_call(self.call_id)
            && !self.provider_id.is_empty()
            && !self.provider_id.contains('\0')
            && self.provider_id.len() <= ToolCallRecord::MAX_LABEL_LEN
            && validate_spawn_sandbox_agent_arguments(&self.arguments)
            && serde_json::from_value::<SpawnSandboxAgentArgs>(self.arguments.clone())
                .is_ok_and(|arguments| arguments.task == self.task)
    }
}

/// One model proposal to wait for an ordered set of admitted sandbox children.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForegroundAgentWaitRequest {
    /// Stable model tool-call identity.
    pub call_id: CallId,
    /// Provider-facing tool-use identity retained for transcript reconstruction.
    pub provider_id: String,
    /// Ordered child identities requested by the model.
    pub child_run_ids: Vec<AgentRunId>,
    /// Canonical closed arguments emitted by the provider.
    pub arguments: Value,
}

impl ForegroundAgentWaitRequest {
    /// Whether immutable provider output agrees with the closed wait contract.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        self.call_id.0 != uuid::Uuid::nil()
            && !self.provider_id.is_empty()
            && !self.provider_id.contains('\0')
            && self.provider_id.len() <= ToolCallRecord::MAX_LABEL_LEN
            && validate_wait_for_agents_arguments(&self.arguments)
            && serde_json::from_value::<WaitForAgentsArgs>(self.arguments.clone())
                .is_ok_and(|arguments| arguments.agent_ids == self.child_run_ids)
    }
}

#[derive(Debug, Default)]
struct AgentProgress {
    usage: Usage,
    model_steps: usize,
}

/// One emission from a durably claimed agent generation.
///
/// Ordinary events still need the worker to append them. A committed event was
/// journaled atomically with another state transition and only needs live
/// publication. Flush barriers let the agent wait until every preceding
/// ordinary event is durable before it performs such a transition.
pub enum ClaimedAgentEvent {
    /// Append this event under its exact attempt ordinal.
    Pending { ordinal: i32, event: AgentEvent },
    /// Publish an event whose journal transaction already committed.
    Committed { ordinal: i32, event: SequencedEvent },
    /// Consume an already committed event ordinal without live publication.
    Recovered { ordinal: i32, event: SequencedEvent },
    /// Acknowledge after all preceding channel items have been handled.
    Flush(oneshot::Sender<()>),
}

enum EventSink<'a> {
    Legacy(&'a UnboundedSender<AgentEvent>),
    Claimed {
        sender: &'a UnboundedSender<ClaimedAgentEvent>,
        next_ordinal: AtomicI32,
    },
}

impl EventSink<'_> {
    fn send(&self, event: AgentEvent) {
        match self {
            Self::Legacy(sender) => {
                let _ = sender.unbounded_send(event);
            }
            Self::Claimed {
                sender,
                next_ordinal,
            } => {
                if let Ok(ordinal) = reserve_event_ordinal(next_ordinal) {
                    let _ = sender.unbounded_send(ClaimedAgentEvent::Pending { ordinal, event });
                }
            }
        }
    }

    async fn flush(&self) -> Result<()> {
        let Self::Claimed { sender, .. } = self else {
            return Ok(());
        };
        let (acknowledge, acknowledged) = oneshot::channel();
        sender
            .unbounded_send(ClaimedAgentEvent::Flush(acknowledge))
            .map_err(|_| AgentError::Store("claimed turn event channel closed".into()))?;
        acknowledged
            .await
            .map_err(|_| AgentError::Store("claimed turn event flush was abandoned".into()))
    }

    fn reserve_ordinal(&self) -> Result<i32> {
        match self {
            Self::Claimed { next_ordinal, .. } => reserve_event_ordinal(next_ordinal),
            Self::Legacy(_) => Err(AgentError::Store(
                "legacy turn cannot reserve a durable event ordinal".into(),
            )),
        }
    }

    fn send_committed(&self, ordinal: i32, event: SequencedEvent) -> Result<()> {
        let Self::Claimed { sender, .. } = self else {
            return Err(AgentError::Store(
                "legacy turn cannot publish a committed durable event".into(),
            ));
        };
        sender
            .unbounded_send(ClaimedAgentEvent::Committed { ordinal, event })
            .map_err(|_| AgentError::Store("claimed turn event channel closed".into()))
    }

    fn proposed_ordinal(&self) -> Result<Option<i32>> {
        match self {
            Self::Legacy(_) => Ok(None),
            Self::Claimed { next_ordinal, .. } => {
                let ordinal = next_ordinal.load(Ordering::SeqCst);
                if !(1..i32::MAX).contains(&ordinal) {
                    return Err(AgentError::Store("turn event ordinal exhausted".into()));
                }
                Ok(Some(ordinal))
            }
        }
    }

    fn send_committed_proposed(&self, ordinal: i32, event: SequencedEvent) -> Result<()> {
        self.send_recovered_or_committed_proposed(ordinal, event, true)
    }

    fn send_recovered_proposed(&self, ordinal: i32, event: SequencedEvent) -> Result<()> {
        self.send_recovered_or_committed_proposed(ordinal, event, false)
    }

    fn send_recovered_or_committed_proposed(
        &self,
        ordinal: i32,
        event: SequencedEvent,
        publish: bool,
    ) -> Result<()> {
        let Self::Claimed {
            sender,
            next_ordinal,
        } = self
        else {
            return Err(AgentError::Store(
                "legacy turn cannot publish a committed durable event".into(),
            ));
        };
        let next = ordinal
            .checked_add(1)
            .filter(|next| *next < i32::MAX)
            .ok_or_else(|| AgentError::Store("turn event ordinal exhausted".into()))?;
        next_ordinal
            .compare_exchange(ordinal, next, Ordering::SeqCst, Ordering::SeqCst)
            .map_err(|_| AgentError::Store("turn event ordinal changed during approval".into()))?;
        sender
            .unbounded_send(if publish {
                ClaimedAgentEvent::Committed { ordinal, event }
            } else {
                ClaimedAgentEvent::Recovered { ordinal, event }
            })
            .map_err(|_| AgentError::Store("claimed turn event channel closed".into()))
    }
}

/// Holds only a suffix that could still become an exact source reference.
/// Non-text provider events that arrive inside that suffix wait with it so an
/// eventual malformed reference can be replayed in its original order.
struct AssistantStreamEventFilter<'a, 'b> {
    sink: &'a EventSink<'b>,
    candidate: String,
    pending: Vec<AgentEvent>,
}

impl<'a, 'b> AssistantStreamEventFilter<'a, 'b> {
    fn new(sink: &'a EventSink<'b>) -> Self {
        Self {
            sink,
            candidate: String::new(),
            pending: Vec::new(),
        }
    }

    fn send(&mut self, event: AgentEvent) {
        if self.candidate.is_empty() {
            self.sink.send(event);
        } else {
            self.pending.push(event);
        }
    }

    fn send_text(&mut self, delta: &str) {
        let mut safe = String::new();
        for character in delta.chars() {
            if self.candidate.is_empty() && character != '[' {
                safe.push(character);
                continue;
            }
            if !safe.is_empty() {
                self.sink.send(AgentEvent::TextDelta {
                    text: std::mem::take(&mut safe),
                });
            }
            self.candidate.push(character);
            self.pending.push(AgentEvent::TextDelta {
                text: character.to_string(),
            });
            self.resolve_candidate();
        }
        if !safe.is_empty() {
            self.sink.send(AgentEvent::TextDelta { text: safe });
        }
    }

    fn resolve_candidate(&mut self) {
        loop {
            match classify_source_reference_candidate(&self.candidate) {
                SourceReferenceCandidate::Possible => return,
                SourceReferenceCandidate::Complete => {
                    self.candidate.clear();
                    for event in self.pending.drain(..) {
                        if !matches!(event, AgentEvent::TextDelta { .. }) {
                            self.sink.send(event);
                        }
                    }
                    return;
                }
                SourceReferenceCandidate::Invalid => {
                    let first_len = self
                        .candidate
                        .chars()
                        .next()
                        .expect("an invalid candidate is nonempty")
                        .len_utf8();
                    self.candidate.drain(..first_len);
                    let first_text = self
                        .pending
                        .iter()
                        .position(|event| matches!(event, AgentEvent::TextDelta { .. }))
                        .expect("each candidate character has a pending text event");
                    for event in self.pending.drain(..=first_text) {
                        self.sink.send(event);
                    }
                    while self
                        .pending
                        .first()
                        .is_some_and(|event| !matches!(event, AgentEvent::TextDelta { .. }))
                    {
                        self.sink.send(self.pending.remove(0));
                    }
                    if self.candidate.is_empty() {
                        debug_assert!(self.pending.is_empty());
                        return;
                    }
                }
            }
        }
    }

    fn finish(&mut self) {
        self.candidate.clear();
        for event in self.pending.drain(..) {
            self.sink.send(event);
        }
    }

    fn discard(&mut self) {
        self.candidate.clear();
        self.pending.clear();
    }
}

fn reserve_event_ordinal(next: &AtomicI32) -> Result<i32> {
    next.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |ordinal| {
        ordinal.checked_add(1).filter(|next| *next < i32::MAX)
    })
    .map_err(|_| AgentError::Store("turn event ordinal exhausted".into()))
}

#[derive(Clone, Copy)]
struct TurnExecution<'a> {
    turn_id: TurnId,
    user_input: &'a str,
    output_message_id: MessageId,
    persist_input: bool,
    publish_started: bool,
    publish_terminal: bool,
}

/// Drives turns for a chat over a provider, tool set, and store.
pub struct Agent {
    provider: Arc<dyn ModelProvider>,
    tools: Arc<ToolRegistry>,
    store: Arc<dyn Store>,
    config: AgentConfig,
    approvals: Arc<dyn ApprovalGate>,
    standing_grants: Arc<StandingGrants>,
    cancel: CancelToken,
    steer: SteerInbox,
    durable_steer_lease: Option<uuid::Uuid>,
    agent_orchestration_enabled: bool,
    continuation_instruction: Option<String>,
}

/// A tool call accumulated from the provider stream.
struct PendingCall {
    call_id: CallId,
    provider_id: String,
    name: String,
    args: String,
}

/// The closed action projection for a pending call, parsed from the arguments
/// it will run with. Arguments that never parsed cannot describe an action.
fn call_action_preview(call: &PendingCall) -> Option<ToolActionPreview> {
    serde_json::from_str(&call.args)
        .ok()
        .and_then(|args| ToolActionPreview::build(&call.name, &args))
}

struct AssistantCandidate {
    content: String,
    citations: Vec<AssistantCitationReference>,
}

enum AcceptedServerCall {
    Accepted,
    Existing(Box<ToolCallRecord>),
    IdentityConflict,
    LeaseLost,
}

impl Agent {
    /// Assemble an agent from its dependencies and config.
    ///
    /// Sensitive tools are refused by default ([`RefuseGate`]). Wire a real
    /// gate with [`with_approvals`](Self::with_approvals) for park-and-resume.
    pub fn new(
        provider: Arc<dyn ModelProvider>,
        tools: Arc<ToolRegistry>,
        store: Arc<dyn Store>,
        config: AgentConfig,
    ) -> Self {
        Self {
            provider,
            tools,
            store,
            config,
            approvals: Arc::new(RefuseGate),
            standing_grants: Arc::new(StandingGrants::new()),
            cancel: CancelToken::new(),
            steer: SteerInbox::new(),
            durable_steer_lease: None,
            agent_orchestration_enabled: false,
            continuation_instruction: None,
        }
    }

    /// Use `gate` for Sensitive-tool decisions (park-and-resume on the server).
    #[must_use]
    pub fn with_approvals(mut self, gate: Arc<dyn ApprovalGate>) -> Self {
        self.approvals = gate;
        self
    }

    /// Consult `grants` before parking a Sensitive tool call, so a repeated
    /// in-scope action the user already approved runs without re-prompting.
    ///
    /// Deny-by-default: an empty set (the default) makes every Sensitive call
    /// park on the gate exactly as before.
    #[must_use]
    pub fn with_standing_grants(mut self, grants: Arc<StandingGrants>) -> Self {
        self.standing_grants = grants;
        self
    }

    /// Watch `cancel` so the turn can be stopped early. Without this the turn
    /// runs to completion (the default token is never tripped).
    #[must_use]
    pub fn with_cancel(mut self, cancel: CancelToken) -> Self {
        self.cancel = cancel;
        self
    }

    /// Drain mid-turn steer messages from `steer`. Without this the turn ignores
    /// any steer pushes (the default inbox stays empty).
    #[must_use]
    pub fn with_steer(mut self, steer: SteerInbox) -> Self {
        self.steer = steer;
        self
    }

    /// Apply durable steering under the exact claimed-turn lease.
    #[must_use]
    pub fn with_durable_steer(mut self, lease_token: uuid::Uuid) -> Self {
        self.durable_steer_lease = Some(lease_token);
        self
    }

    /// Advertise and accept foreground-only spawn and ordered-wait tools.
    ///
    /// This is intentionally opt-in: sandbox workers must not set it, keeping
    /// the v1 hierarchy at a single child depth.
    #[must_use]
    pub fn with_foreground_agent_orchestration(mut self) -> Self {
        self.agent_orchestration_enabled = true;
        self
    }

    /// Add a fixed runtime correction before the next provider invocation.
    #[must_use]
    pub fn with_continuation_instruction(mut self, instruction: Option<String>) -> Self {
        self.continuation_instruction = instruction;
        self
    }

    fn agent_orchestration_active(&self) -> bool {
        self.agent_orchestration_enabled && self.durable_steer_lease.is_some()
    }

    /// Run one turn: submit `user_input`, drive the loop to a final answer,
    /// streaming [`AgentEvent`]s to `events`.
    ///
    /// Returns `Err` (after emitting `TurnFailed`) on an infrastructure failure
    /// (provider, store) or when the step guard is exceeded. Tool failures are
    /// not errors — they come back to the model as failed tool output.
    pub async fn run_turn(
        &self,
        chat: &Chat,
        user_input: &str,
        events: &UnboundedSender<AgentEvent>,
    ) -> Result<()> {
        let turn_id = TurnId::new();
        let output_message_id = MessageId::new();
        let events = EventSink::Legacy(events);
        self.run_turn_inner(
            chat,
            TurnExecution {
                turn_id,
                user_input,
                output_message_id,
                persist_input: true,
                publish_started: true,
                publish_terminal: true,
            },
            &events,
        )
        .await
        .map(|_| ())
    }

    /// Execute an exact durably claimed turn without duplicating its accepted
    /// input or publishing its final output ahead of the terminal state change.
    ///
    /// `turn_id` identifies the already-persisted [`crate::TurnRun`], whose
    /// accepted user input must already be present in the store.
    /// `output_message_id` is the worker's stable completion identity and is returned in
    /// [`AgentTurnOutcome::Completed`] for an atomic
    /// [`Store::complete_turn_run`] call. Intermediate assistant/tool state is
    /// still persisted as it is produced so a later turn can rebuild context.
    /// Terminal completed/cancelled/failed events are left to the worker to
    /// publish only after its matching durable state transition commits.
    pub async fn run_claimed_turn(
        &self,
        chat: &Chat,
        turn_id: TurnId,
        output_message_id: MessageId,
        first_event_ordinal: i32,
        events: &UnboundedSender<ClaimedAgentEvent>,
    ) -> Result<AgentTurnOutcome> {
        if turn_id.0.is_nil()
            || output_message_id.0.is_nil()
            || !(1..i32::MAX).contains(&first_event_ordinal)
        {
            return Err(AgentError::Store(
                "claimed turn identities and first event ordinal must be valid".into(),
            ));
        }
        let events = EventSink::Claimed {
            sender: events,
            next_ordinal: AtomicI32::new(first_event_ordinal),
        };
        self.run_turn_inner(
            chat,
            TurnExecution {
                turn_id,
                user_input: "",
                output_message_id,
                persist_input: false,
                publish_started: false,
                publish_terminal: false,
            },
            &events,
        )
        .await
    }

    async fn run_turn_inner(
        &self,
        chat: &Chat,
        execution: TurnExecution<'_>,
        events: &EventSink<'_>,
    ) -> Result<AgentTurnOutcome> {
        if execution.publish_started {
            events.send(AgentEvent::TurnStarted {
                turn_id: execution.turn_id,
            });
        }
        let mut progress = AgentProgress::default();
        match self.drive(chat, execution, events, &mut progress).await {
            Ok(outcome) => {
                if execution.publish_terminal {
                    if let AgentTurnOutcome::Failed { error, .. } = &outcome {
                        events.send(AgentEvent::TurnFailed {
                            error: error.clone(),
                        });
                    }
                }
                Ok(outcome)
            }
            Err(err) => {
                if execution.publish_terminal {
                    events.send(AgentEvent::TurnFailed {
                        error: (&err).into(),
                    });
                } else if progress.model_steps > 0 {
                    return Ok(AgentTurnOutcome::Failed {
                        error: (&err).into(),
                        usage: progress.usage,
                        model_steps: progress.model_steps,
                    });
                }
                Err(err)
            }
        }
    }

    async fn drive(
        &self,
        chat: &Chat,
        execution: TurnExecution<'_>,
        events: &EventSink<'_>,
        progress: &mut AgentProgress,
    ) -> Result<AgentTurnOutcome> {
        let TurnExecution {
            turn_id,
            user_input,
            output_message_id,
            persist_input,
            publish_terminal,
            ..
        } = execution;
        if persist_input {
            self.persist(chat.id, turn_id, Role::User, user_input)
                .await?;
        }
        // The provider transcript for this turn: prior stored text + the blocks
        // we build up as the loop runs.
        let mut transcript = self.load_transcript(chat.id).await?;
        if let Some(instruction) = self.continuation_instruction.as_ref() {
            transcript.push(ChatMessage::text(Role::System, instruction.clone()));
        }
        let mut total_usage = Usage::default();
        self.resume_pending_server_calls(chat, turn_id, events, &mut transcript)
            .await?;
        let mut reduction_level: u32 = 0;

        for step in 0..self.config.max_steps {
            // Between steps: stop before starting a fresh model call if cancelled.
            if self.cancel.is_cancelled() {
                return Ok(self.finish_cancelled(events, total_usage, step, publish_terminal));
            }
            // Boundary steer: inject any queued messages before the next model call.
            self.apply_steers(chat, turn_id, &mut transcript, None, events)
                .await?;
            // Fence this exact provider request, not the later worker handoff.
            // A steer applied after this snapshot must supersede its output;
            // one applied at the boundary above is already part of the prompt.
            let generation_steer_revision = self.durable_generation_revision(turn_id).await?;

            // Fit the transcript to the context window, retrying with tighter
            // budgets on prompt-too-long errors from the provider.
            let mut stream = loop {
                let (fitted, reduced) = self.fit_transcript(&transcript, reduction_level);
                let fitted_tokens = context::estimate_transcript_tokens(&fitted);
                let request = ChatRequest {
                    provider: self.config.provider.clone(),
                    model: self.config.model.clone(),
                    reasoning_model: self.config.reasoning_model,
                    system: self.config.system_prompt.clone(),
                    messages: fitted,
                    tools: self
                        .tools
                        .specs_for_foreground(self.agent_orchestration_active()),
                    max_tokens: self.config.max_tokens,
                    temperature: self.config.temperature,
                    reasoning_effort: self.config.reasoning_effort,
                    // Empty until message attachments are persisted and the
                    // transcript can carry image blocks; hydration from the
                    // blob store hangs here.
                    images: crate::image::ImageAttachments::new(),
                };

                progress.model_steps = step + 1;
                match self.provider.stream(request).await {
                    Ok(stream) => {
                        // Tell clients the history was shortened for this call so
                        // a UI can surface it. Emitted only for the request that
                        // actually went out (after any retry climb).
                        if reduced {
                            events.send(AgentEvent::ContextTruncated {
                                original_tokens: context::estimate_transcript_tokens(&transcript)
                                    as u32,
                                fitted_tokens: fitted_tokens as u32,
                            });
                        }
                        reduction_level = 0;
                        break stream;
                    }
                    Err(AgentError::PromptTooLong(_))
                        if reduction_level < context::MAX_REDUCTION_LEVEL =>
                    {
                        reduction_level += 1;
                    }
                    Err(e) => return Err(e),
                }
            };
            let mut text = String::new();
            let mut calls: Vec<PendingCall> = Vec::new();
            let mut by_index: HashMap<u32, usize> = HashMap::new();
            let mut stop_reason = StopReason::EndTurn;

            // Race each stream item against cancel and interrupt-steer so a long
            // model call is preempted promptly. Cancel ends the turn; interrupt
            // discards this step's partial output and continues after injecting.
            enum StreamEnd {
                Done,
                Cancelled,
                Steered,
                Failed(String),
            }
            let mut streamed_events = AssistantStreamEventFilter::new(events);
            let stream_end = loop {
                let event = match future::select(
                    stream.next(),
                    future::select(self.cancel.cancelled(), self.steer.interrupted()),
                )
                .await
                {
                    Either::Left((Some(event), _)) => event,
                    Either::Left((None, _)) => break StreamEnd::Done,
                    Either::Right((Either::Left(((), _)), _)) => break StreamEnd::Cancelled,
                    Either::Right((Either::Right(((), _)), _)) => break StreamEnd::Steered,
                };
                match event {
                    ProviderEvent::TextDelta { text: delta } => {
                        text.push_str(&delta);
                        streamed_events.send_text(&delta);
                    }
                    ProviderEvent::ReasoningDelta { text: delta } => {
                        streamed_events.send(AgentEvent::ReasoningDelta { text: delta });
                    }
                    ProviderEvent::ToolCallStarted { index, id, name } => {
                        let call_id = CallId::new();
                        streamed_events.send(AgentEvent::ToolCallStarted {
                            call_id,
                            name: name.clone(),
                        });
                        by_index.insert(index, calls.len());
                        calls.push(PendingCall {
                            call_id,
                            provider_id: id,
                            name,
                            args: String::new(),
                        });
                    }
                    ProviderEvent::ToolCallArgsDelta { index, fragment } => {
                        if let Some(&i) = by_index.get(&index) {
                            streamed_events.send(AgentEvent::ToolCallArgsDelta {
                                call_id: calls[i].call_id,
                                fragment: fragment.clone(),
                            });
                            calls[i].args.push_str(&fragment);
                        }
                    }
                    ProviderEvent::Usage(reported) => {
                        total_usage = total_usage.checked_add(reported).ok_or_else(|| {
                            AgentError::msg("provider usage exceeded the supported turn total")
                        })?;
                        progress.usage = total_usage;
                    }
                    ProviderEvent::Stop { reason } => stop_reason = reason,
                    ProviderEvent::Failed { message } => break StreamEnd::Failed(message),
                }
            };
            if matches!(stream_end, StreamEnd::Steered | StreamEnd::Failed(_)) {
                streamed_events.discard();
            } else {
                // Normal completion and cancellation retain malformed or
                // incomplete marker-like prose exactly. Only a steer or a
                // broken stream discards the entire candidate under
                // StreamInterrupted semantics.
                streamed_events.finish();
            }
            // A stream that broke mid-flight left this step's tool-call
            // arguments possibly truncated mid-JSON. Nothing here is safe to
            // act on, and nothing was persisted, so fail the turn under a
            // retryable provider code rather than executing the fragment.
            if let StreamEnd::Failed(message) = stream_end {
                events.send(AgentEvent::StreamInterrupted);
                return Err(AgentError::Provider(message));
            }
            // Prefer cancel when both cancel and interrupt are ready (cancel is
            // the left arm of the nested select). Also catch a cancel that raced
            // the final stream event.
            if matches!(stream_end, StreamEnd::Cancelled) || self.cancel.is_cancelled() {
                return Ok(self.finish_cancelled(events, total_usage, step + 1, publish_terminal));
            }
            if matches!(stream_end, StreamEnd::Steered) {
                // Discard this step's partial output — nothing from it was
                // persisted. The marker lets replay/live clients clear deltas
                // that were already streamed for this abandoned provider step.
                events.send(AgentEvent::StreamInterrupted);
                self.apply_steers(chat, turn_id, &mut transcript, None, events)
                    .await?;
                continue;
            }

            let parsed = parse_assistant_citations(&text);
            let candidate = AssistantCandidate {
                content: parsed.content,
                citations: parsed.references,
            };
            let text = &candidate.content;

            let unavailable_foreground_client = calls.iter().any(|call| {
                self.tools.is_foreground_client(&call.name) && !self.agent_orchestration_active()
            });
            if unavailable_foreground_client {
                events.send(AgentEvent::StreamInterrupted);
                transcript.push(ChatMessage::text(
                    Role::User,
                    "That user continuation is available only from a durably claimed foreground turn. Continue without requesting it.",
                ));
                continue;
            }
            let client_calls = calls
                .iter()
                .filter(|call| self.tools.execution(&call.name) == Some(ToolCallExecution::Client))
                .collect::<Vec<_>>();
            if !client_calls.is_empty() {
                // Prose is kept, exactly as for a sensitive call (#372): the
                // model narrates before it acts, and rejecting the step for
                // that alone burned the step budget on a correction it was
                // never going to satisfy. Only sibling calls stay forbidden —
                // the checkpoint can carry one call across the resume.
                if calls.len() != 1 || client_calls.len() != 1 {
                    events.send(AgentEvent::StreamInterrupted);
                    transcript.push(ChatMessage {
                        role: Role::User,
                        content: vec![ContentBlock::Text {
                            text: "A client-executed tool must be requested alone, without sibling tool calls. Retry the request in that form.".into(),
                        }],
                    });
                    continue;
                }
                let call = client_calls[0];
                let Some(arguments) = parse_client_args(&call.args) else {
                    events.send(AgentEvent::StreamInterrupted);
                    transcript.push(ChatMessage {
                        role: Role::User,
                        content: vec![ContentBlock::Text {
                            text: "The client tool arguments were not valid JSON. Retry the request with a complete JSON value.".into(),
                        }],
                    });
                    continue;
                };
                let request = crate::model::ClientToolCallRequest {
                    id: call.call_id,
                    chat_id: chat.id,
                    turn_id,
                    provider_id: call.provider_id.clone(),
                    name: call.name.clone(),
                    arguments,
                };
                if !request.is_well_formed()
                    || !self
                        .tools
                        .client_arguments_are_valid(&request.name, &request.arguments)
                {
                    events.send(AgentEvent::StreamInterrupted);
                    transcript.push(ChatMessage {
                        role: Role::User,
                        content: vec![ContentBlock::Text {
                            text: "The client tool request was too large or malformed. Retry it with a valid tool identity and smaller arguments.".into(),
                        }],
                    });
                    continue;
                }
                let steer_revision = generation_steer_revision.ok_or_else(|| {
                    AgentError::Store("client-executed tools require a durably claimed turn".into())
                })?;
                // The checkpoint returns from the loop and the resumed attempt
                // rebuilds its transcript from the store, so an unpersisted
                // preamble would be lost. Fence on the lease first: the stream
                // just consumed may have outlasted it.
                if !text.is_empty() {
                    self.ensure_durable_lease_current(turn_id).await?;
                    self.persist_assistant(chat.id, turn_id, &candidate).await?;
                }
                return Ok(AgentTurnOutcome::ClientToolCall {
                    request,
                    usage: total_usage,
                    steer_revision,
                    model_steps: step + 1,
                });
            }

            let sandbox_spawns = calls
                .iter()
                .filter(|call| {
                    self.agent_orchestration_active()
                        && self.tools.is_foreground_sandbox_spawn(&call.name)
                })
                .collect::<Vec<_>>();
            if !sandbox_spawns.is_empty() {
                if calls.len() != 1 || sandbox_spawns.len() != 1 {
                    events.send(AgentEvent::StreamInterrupted);
                    transcript.push(ChatMessage {
                        role: Role::User,
                        content: vec![ContentBlock::Text {
                            text: "A sandbox delegation must be requested alone, without sibling tool calls. Retry the request in that form.".into(),
                        }],
                    });
                    continue;
                }
                let call = sandbox_spawns[0];
                let Some(arguments) = parse_client_args(&call.args) else {
                    events.send(AgentEvent::StreamInterrupted);
                    transcript.push(ChatMessage {
                        role: Role::User,
                        content: vec![ContentBlock::Text {
                            text: "The sandbox task arguments were not valid JSON. Retry with one complete task value.".into(),
                        }],
                    });
                    continue;
                };
                let Some(task) = self.tools.sandbox_spawn_task(&call.name, &arguments) else {
                    events.send(AgentEvent::StreamInterrupted);
                    transcript.push(ChatMessage {
                        role: Role::User,
                        content: vec![ContentBlock::Text {
                            text: "The sandbox task needs one non-empty, bounded `task`. It may also include one `resource` object containing only `root_id` and `relative_path`; omit `resource` entirely when unused rather than sending null. Retry with that exact shape.".into(),
                        }],
                    });
                    continue;
                };
                let Some(steer_revision) = generation_steer_revision else {
                    events.send(AgentEvent::StreamInterrupted);
                    transcript.push(ChatMessage {
                        role: Role::User,
                        content: vec![ContentBlock::Text {
                            text: "Sandbox delegation is available only from a durably claimed foreground turn. Continue without delegating.".into(),
                        }],
                    });
                    continue;
                };
                if !text.is_empty() {
                    self.ensure_durable_lease_current(turn_id).await?;
                    self.persist_assistant(chat.id, turn_id, &candidate).await?;
                }
                return Ok(AgentTurnOutcome::SandboxAgentSpawn {
                    request: SandboxAgentSpawnRequest {
                        call_id: call.call_id,
                        provider_id: call.provider_id.clone(),
                        child_run_id: AgentRunId::sandbox_for_spawn_call(call.call_id),
                        task,
                        arguments,
                    },
                    usage: total_usage,
                    steer_revision,
                    model_steps: step + 1,
                });
            }

            let agent_waits = calls
                .iter()
                .filter(|call| {
                    self.agent_orchestration_active()
                        && self.tools.is_foreground_agent_wait(&call.name)
                })
                .collect::<Vec<_>>();
            if !agent_waits.is_empty() {
                if calls.len() != 1 || agent_waits.len() != 1 {
                    events.send(AgentEvent::StreamInterrupted);
                    transcript.push(ChatMessage::text(
                        Role::User,
                        "wait_for_agents must be requested alone, without sibling tool calls. Retry the request in that form.",
                    ));
                    continue;
                }
                let call = agent_waits[0];
                let Some(arguments) = parse_client_args(&call.args) else {
                    events.send(AgentEvent::StreamInterrupted);
                    transcript.push(ChatMessage::text(
                        Role::User,
                        "The wait_for_agents arguments were not valid JSON. Retry with one complete ordered agent_ids value.",
                    ));
                    continue;
                };
                let Some(child_run_ids) = self.tools.wait_for_agent_ids(&call.name, &arguments)
                else {
                    events.send(AgentEvent::StreamInterrupted);
                    transcript.push(ChatMessage::text(
                        Role::User,
                        "wait_for_agents requires one non-empty, bounded, unique agent_ids list with no extra properties.",
                    ));
                    continue;
                };
                let Some(steer_revision) = generation_steer_revision else {
                    events.send(AgentEvent::StreamInterrupted);
                    transcript.push(ChatMessage::text(
                        Role::User,
                        "wait_for_agents is available only from a durably claimed foreground turn.",
                    ));
                    continue;
                };
                if !text.is_empty() {
                    self.ensure_durable_lease_current(turn_id).await?;
                    self.persist_assistant(chat.id, turn_id, &candidate).await?;
                }
                return Ok(AgentTurnOutcome::WaitForAgents {
                    request: ForegroundAgentWaitRequest {
                        call_id: call.call_id,
                        provider_id: call.provider_id.clone(),
                        child_run_ids,
                        arguments,
                    },
                    usage: total_usage,
                    steer_revision,
                    model_steps: step + 1,
                });
            }

            let sensitive_calls = calls
                .iter()
                .filter(|call| {
                    self.tools
                        .get(&call.name)
                        .is_some_and(|tool| tool.approval_class() == ApprovalClass::Sensitive)
                })
                .collect::<Vec<_>>();
            // A sensitive call may carry assistant prose ("I'll search for…"):
            // the step then persists its message and parks exactly like any
            // other text+tool step — three lease-fenced writes, with recovery
            // abandoning an interrupted call behind its visible preamble. Only
            // sibling calls stay forbidden: a multi-call sensitive batch has no
            // unambiguous resume shape (see resume_pending_server_calls).
            // Rejecting prose here used to burn the whole step budget on
            // corrective retries the model never satisfied (#372).
            if !sensitive_calls.is_empty() && (calls.len() != 1 || sensitive_calls.len() != 1) {
                events.send(AgentEvent::StreamInterrupted);
                transcript.push(ChatMessage {
                    role: Role::User,
                    content: vec![ContentBlock::Text {
                        text: "A sensitive tool must be requested alone, without sibling tool calls. Retry the request in that form.".into(),
                    }],
                });
                continue;
            }

            // This step is about to persist tool-call rows, execute server tool
            // side effects, and record the assistant message. Fence those on the
            // lease first: the provider stream just consumed may have outlasted
            // it, and a stale segment must neither commit nor replay an effect a
            // later attempt now owns. Terminal completion is left to the worker's
            // own lease compare-and-swap, so only fence the tool-bearing path.
            if !calls.is_empty() {
                self.ensure_durable_lease_current(turn_id).await?;
            }

            // Record the assistant message (text + any tool-use blocks).
            let mut blocks: Vec<ContentBlock> = Vec::new();
            if !text.is_empty() {
                blocks.push(ContentBlock::Text { text: text.clone() });
                if !calls.is_empty() {
                    self.persist_assistant(chat.id, turn_id, &candidate).await?;
                }
            }
            let mut recovered_results: HashMap<CallId, ToolOutput> = HashMap::new();
            for call in &calls {
                let args = parse_args(&call.args);
                blocks.push(ContentBlock::ToolUse {
                    id: call.provider_id.clone(),
                    name: call.name.clone(),
                    input: args.clone(),
                });
                // Persist the call as soon as args are known so a crash mid-tool
                // still leaves a reconstructable ToolUse on the next turn.
                let record = ToolCallRecord {
                    id: call.call_id,
                    chat_id: chat.id,
                    turn_id,
                    provider_id: call.provider_id.clone(),
                    name: call.name.clone(),
                    arguments: args,
                    execution: ToolCallExecution::Server,
                    status: ToolCallStatus::Pending,
                    result: None,
                    error_code: None,
                    error_detail: None,
                    client_executor_id: None,
                    client_lease_expires_at: None,
                    created_at: Utc::now(),
                    resolved_at: None,
                };
                let outcome = self.accept_server_call_retry(&record).await?;
                match outcome {
                    AcceptedServerCall::Accepted => {}
                    AcceptedServerCall::Existing(existing) if existing.status.is_terminal() => {
                        let content = existing.result.ok_or_else(|| {
                            AgentError::Store(format!(
                                "terminal tool call {} is missing its result",
                                call.call_id
                            ))
                        })?;
                        recovered_results.insert(
                            call.call_id,
                            ToolOutput {
                                content,
                                data: None,
                                is_error: existing.status != ToolCallStatus::Completed,
                                private_evidence: Vec::new(),
                            },
                        );
                    }
                    AcceptedServerCall::Existing(_) => {}
                    AcceptedServerCall::IdentityConflict => {
                        return Err(AgentError::Store(format!(
                            "tool call {} identity conflicts with its canonical request",
                            call.call_id
                        )));
                    }
                    AcceptedServerCall::LeaseLost => {
                        return Err(AgentError::Store(format!(
                            "turn {turn_id} lost its lease while accepting tool call {}",
                            call.call_id
                        )));
                    }
                }
            }
            if !blocks.is_empty() {
                transcript.push(ChatMessage {
                    role: Role::Assistant,
                    content: blocks,
                });
            }

            if calls.is_empty() {
                // A boundary steer can turn this final candidate into an
                // intermediate assistant message. Legacy turns persist each
                // candidate immediately, so each needs its own identity. A
                // claimed turn keeps the caller's stable completion identity:
                // steered candidates are persisted separately by
                // `apply_steers`, and only the actual final output uses it.
                let candidate_message_id = if publish_terminal {
                    MessageId::new()
                } else {
                    output_message_id
                };
                let output = Message {
                    id: candidate_message_id,
                    chat_id: chat.id,
                    turn_id,
                    role: Role::Assistant,
                    content: text.clone(),
                    created_at: Utc::now(),
                };
                if publish_terminal && !text.is_empty() {
                    self.append_assistant_exact_retry(&output, &candidate.citations)
                        .await?;
                }
                // Drain steers until the inbox is quiet, then complete. A steer
                // that arrives as the stream finished must continue the turn
                // rather than race a TurnCompleted. `try_complete` holds the
                // queue lock across the empty-check and terminal emit so a
                // concurrent push cannot 202 and then be orphaned.
                loop {
                    if self.cancel.is_cancelled() {
                        return Ok(self.finish_cancelled(
                            events,
                            total_usage,
                            step + 1,
                            publish_terminal,
                        ));
                    }
                    if self
                        .apply_steers(
                            chat,
                            turn_id,
                            &mut transcript,
                            (!publish_terminal && !text.is_empty()).then_some(&candidate),
                            events,
                        )
                        .await?
                    {
                        break; // continue the outer step loop below
                    }
                    if self.durable_steer_lease.is_some() {
                        return Ok(AgentTurnOutcome::Completed {
                            output,
                            citations: candidate.citations.clone(),
                            usage: total_usage,
                            stop_reason,
                            steer_revision: generation_steer_revision,
                            model_steps: step + 1,
                        });
                    }
                    if self.steer.try_complete(|| {
                        if publish_terminal {
                            events.send(AgentEvent::TurnCompleted {
                                usage: total_usage,
                                stop_reason,
                            });
                        }
                    }) {
                        return Ok(AgentTurnOutcome::Completed {
                            output,
                            citations: candidate.citations.clone(),
                            usage: total_usage,
                            stop_reason,
                            steer_revision: generation_steer_revision,
                            model_steps: step + 1,
                        });
                    }
                    // Steer arrived between drain and try_complete — loop.
                }
                continue;
            }

            // Tool calls need a following model call to consume their results. If
            // no step remains, stop now — running them would be wasted side
            // effects the model can never act on.
            if step + 1 >= self.config.max_steps {
                return Err(AgentError::msg("max steps per turn exceeded"));
            }

            // Run the tool calls and feed the results back for the next step.
            let mut results: Vec<ContentBlock> = Vec::new();
            for call in &calls {
                let (output, needs_resolution) = match recovered_results.remove(&call.call_id) {
                    Some(output) => (output, false),
                    None if self.cancel.is_cancelled() => (
                        ToolOutput::error("turn cancelled before tool execution"),
                        true,
                    ),
                    None => {
                        self.ensure_durable_lease_current(turn_id).await?;
                        (self.run_tool(chat, turn_id, call, events, None).await, true)
                    }
                };
                events.send(AgentEvent::ToolCallCompleted {
                    call_id: call.call_id,
                    output: output.clone(),
                    action: call_action_preview(call),
                    result: ToolResultPreview::build(&call.name, output.data.as_ref()),
                });
                if needs_resolution {
                    let resolution = if output.is_error {
                        ToolCallResolution::Failed {
                            result: output.content.clone(),
                            error_code: "tool_error".into(),
                            error_detail: None,
                        }
                    } else {
                        ToolCallResolution::Completed {
                            result: output.content.clone(),
                        }
                    };
                    let outcome = self
                        .resolve_server_call_retry(
                            chat.id,
                            turn_id,
                            call.call_id,
                            &resolution,
                            &output.private_evidence,
                        )
                        .await?;
                    if !matches!(
                        outcome,
                        ResolveToolCallOutcome::Resolved | ResolveToolCallOutcome::Existing
                    ) {
                        return Err(AgentError::Store(format!(
                            "tool call {} could not be resolved: {outcome:?}",
                            call.call_id
                        )));
                    }
                }
                results.push(ContentBlock::ToolResult {
                    tool_use_id: call.provider_id.clone(),
                    content: output.content,
                    is_error: output.is_error,
                });
                // A cancel that arrived during this tool (including while it was
                // parked on approval) stops the turn before the next model call.
                if self.cancel.is_cancelled() {
                    return Ok(self.finish_cancelled(
                        events,
                        total_usage,
                        step + 1,
                        publish_terminal,
                    ));
                }
            }
            // Tool results ride in a user-role message (the Messages convention).
            transcript.push(ChatMessage {
                role: Role::User,
                content: results,
            });
            // Boundary steer after tools — injected before the next model step.
            self.apply_steers(chat, turn_id, &mut transcript, None, events)
                .await?;
        }

        if self.config.max_steps == 0 {
            return Err(AgentError::msg("max steps per turn exceeded"));
        }
        Ok(AgentTurnOutcome::Failed {
            error: crate::error::AgentErrorInfo {
                kind: "max_steps_exceeded".into(),
                message: "max steps per turn exceeded".into(),
            },
            usage: total_usage,
            model_steps: self.config.max_steps,
        })
    }

    /// Emit the cancellation terminal event and end the turn as a (non-error)
    /// success — the client asked for the stop, so it isn't a `TurnFailed`.
    fn finish_cancelled(
        &self,
        events: &EventSink<'_>,
        usage: Usage,
        model_steps: usize,
        publish_terminal_event: bool,
    ) -> AgentTurnOutcome {
        if publish_terminal_event {
            events.send(AgentEvent::TurnCancelled { usage });
        }
        AgentTurnOutcome::Cancelled { usage, model_steps }
    }

    /// Drain the steer inbox into the transcript. Returns whether any messages
    /// were injected. Emits [`AgentEvent::UserSteered`] per message.
    async fn apply_steers(
        &self,
        chat: &Chat,
        turn_id: TurnId,
        transcript: &mut Vec<ChatMessage>,
        preceding_assistant: Option<&AssistantCandidate>,
        events: &EventSink<'_>,
    ) -> Result<bool> {
        let msgs = self.steer.drain();
        let durable = match self.durable_steer_lease {
            Some(lease_token) => self.list_durable_steers_retry(turn_id, lease_token).await?,
            None => Vec::new(),
        };
        if msgs.is_empty() && durable.is_empty() {
            return Ok(false);
        }
        if self.durable_steer_lease.is_some() && !msgs.is_empty() {
            return Err(AgentError::Store(format!(
                "turn {turn_id} mixed process-local messages with durable steering"
            )));
        }
        if self.durable_steer_lease.is_none() {
            if let Some(candidate) = preceding_assistant {
                self.persist_assistant(chat.id, turn_id, candidate).await?;
            }
        }
        for msg in msgs {
            let message_id = self
                .persist(chat.id, turn_id, Role::User, &msg.content)
                .await?;
            transcript.push(ChatMessage {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: msg.content.clone(),
                }],
            });
            events.send(AgentEvent::UserSteered {
                message_id,
                content: msg.content,
            });
        }
        let preceding = preceding_assistant
            .filter(|candidate| !candidate.content.is_empty() && !durable.is_empty())
            .map(|candidate| Message {
                id: MessageId::new(),
                chat_id: chat.id,
                turn_id,
                role: Role::Assistant,
                content: candidate.content.clone(),
                created_at: Utc::now(),
            });
        let preceding_citations = preceding_assistant
            .filter(|candidate| !candidate.content.is_empty() && !durable.is_empty())
            .map_or(&[][..], |candidate| candidate.citations.as_slice());
        if !durable.is_empty() {
            events.flush().await?;
        }
        let lease_token = self.durable_steer_lease;
        for (index, steer) in durable.into_iter().enumerate() {
            let preceding_assistant = if index == 0 { preceding.as_ref() } else { None };
            let event_ordinal = events.reserve_ordinal()?;
            let journaled = self
                .apply_durable_steer_retry(
                    turn_id,
                    lease_token.expect("durable steering has a lease"),
                    steer.id,
                    event_ordinal,
                    preceding_assistant,
                    if index == 0 { preceding_citations } else { &[] },
                )
                .await?;
            let steer = match journaled.outcome {
                ApplyTurnSteerOutcome::Applied(steer) | ApplyTurnSteerOutcome::Existing(steer) => {
                    steer
                }
            };
            let event = journaled.event;
            transcript.push(ChatMessage {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: steer.content.clone(),
                }],
            });
            events.send_committed(event_ordinal, event)?;
        }
        Ok(true)
    }

    /// Confirm the durable lease still owns this turn before the current model
    /// step commits or replays any intermediate tool or message effect.
    ///
    /// The per-step generation fence proves the lease before the provider call,
    /// but a long provider stream can outlast the lease; once it expires another
    /// worker may terminalize or reclaim the turn. Re-checking here keeps a
    /// segment whose lease was stolen mid-stream from writing tool-call rows,
    /// executing filesystem or external side effects, or persisting messages a
    /// later attempt now owns. Legacy (unclaimed) turns carry no lease and are
    /// never fenced.
    async fn ensure_durable_lease_current(&self, turn_id: TurnId) -> Result<()> {
        let Some(lease_token) = self.durable_steer_lease else {
            return Ok(());
        };
        loop {
            match self
                .store
                .fence_turn_lease(turn_id, lease_token, Utc::now())
                .await
            {
                Ok(TurnLeaseFence::Current) => return Ok(()),
                Ok(TurnLeaseFence::Stale) => {
                    return Err(AgentError::Store(format!(
                        "turn {turn_id} no longer owns lease {lease_token}; refusing to commit intermediate effects"
                    )));
                }
                Err(_) => self.wait_for_durable_store_retry(turn_id).await?,
            }
        }
    }

    async fn accept_server_call_retry(&self, call: &ToolCallRecord) -> Result<AcceptedServerCall> {
        let Some(lease_token) = self.durable_steer_lease else {
            return Ok(match self.store.accept_tool_call(call).await? {
                AcceptToolCallOutcome::Accepted(_) => AcceptedServerCall::Accepted,
                AcceptToolCallOutcome::Existing(existing) => {
                    AcceptedServerCall::Existing(Box::new(existing))
                }
                AcceptToolCallOutcome::IdentityConflict => AcceptedServerCall::IdentityConflict,
            });
        };
        loop {
            match self
                .store
                .accept_claimed_tool_call(call, lease_token, Utc::now())
                .await
            {
                Ok(AcceptClaimedToolCallOutcome::Accepted(_)) => {
                    return Ok(AcceptedServerCall::Accepted);
                }
                Ok(AcceptClaimedToolCallOutcome::Existing(existing)) => {
                    return Ok(AcceptedServerCall::Existing(Box::new(existing)));
                }
                Ok(AcceptClaimedToolCallOutcome::IdentityConflict) => {
                    return Ok(AcceptedServerCall::IdentityConflict);
                }
                Ok(AcceptClaimedToolCallOutcome::LeaseLost) => {
                    return Ok(AcceptedServerCall::LeaseLost);
                }
                Err(_) => {
                    self.ensure_durable_lease_current(call.turn_id).await?;
                    self.wait_for_durable_store_retry(call.turn_id).await?;
                }
            }
        }
    }

    async fn resolve_server_call_retry(
        &self,
        chat_id: ChatId,
        turn_id: TurnId,
        call_id: CallId,
        resolution: &ToolCallResolution,
        evidence: &[crate::RetrievalEvidenceInput],
    ) -> Result<ResolveToolCallOutcome> {
        let resolved_at = Utc::now();
        let Some(lease_token) = self.durable_steer_lease else {
            return self
                .store
                .resolve_server_tool_call_with_evidence(call_id, resolution, resolved_at, evidence)
                .await;
        };
        loop {
            match self
                .store
                .resolve_claimed_server_tool_call_with_evidence(
                    call_id,
                    chat_id,
                    turn_id,
                    lease_token,
                    Utc::now(),
                    resolution,
                    resolved_at,
                    evidence,
                )
                .await
            {
                Ok(outcome) => return Ok(outcome),
                Err(_) => {
                    self.ensure_durable_lease_current(turn_id).await?;
                    self.wait_for_durable_store_retry(turn_id).await?;
                }
            }
        }
    }

    async fn abandon_inherited_server_call_retry(
        &self,
        chat_id: ChatId,
        turn_id: TurnId,
        call_id: CallId,
        resolution: &ToolCallResolution,
    ) -> Result<ResolveToolCallOutcome> {
        let lease_token = self.durable_steer_lease.ok_or_else(|| {
            AgentError::Store("inherited tool abandonment requires a durable lease".into())
        })?;
        let resolved_at = Utc::now();
        loop {
            match self
                .store
                .abandon_inherited_server_tool_call(
                    call_id,
                    chat_id,
                    turn_id,
                    lease_token,
                    Utc::now(),
                    resolution,
                    resolved_at,
                )
                .await
            {
                Ok(outcome) => return Ok(outcome),
                Err(_) => {
                    self.ensure_durable_lease_current(turn_id).await?;
                    self.wait_for_durable_store_retry(turn_id).await?;
                }
            }
        }
    }

    async fn durable_generation_revision(&self, turn_id: TurnId) -> Result<Option<i64>> {
        match self.durable_steer_lease {
            Some(lease_token) => self
                .durable_turn_revision_retry(turn_id, lease_token)
                .await
                .map(Some),
            None => Ok(None),
        }
    }

    async fn durable_turn_revision_retry(
        &self,
        turn_id: TurnId,
        lease_token: uuid::Uuid,
    ) -> Result<i64> {
        loop {
            match self.store.get_turn_run(turn_id).await {
                Ok(Some(turn))
                    if turn.status == TurnRunStatus::Running
                        && turn.lease_token == Some(lease_token)
                        && turn
                            .lease_expires_at
                            .is_some_and(|expires_at| expires_at > Utc::now()) =>
                {
                    return Ok(turn.steer_revision);
                }
                Ok(_) => {
                    return Err(AgentError::Store(format!(
                        "turn {turn_id} no longer has live lease {lease_token}"
                    )));
                }
                Err(_) => self.wait_for_durable_store_retry(turn_id).await?,
            }
        }
    }

    async fn list_durable_steers_retry(
        &self,
        turn_id: TurnId,
        lease_token: uuid::Uuid,
    ) -> Result<Vec<crate::model::TurnSteer>> {
        loop {
            match self
                .store
                .list_pending_turn_steers(turn_id, lease_token, Utc::now())
                .await
            {
                Ok(Some(steers)) => return Ok(steers),
                Ok(None) | Err(_) => {
                    // Heartbeat and admission both advance `updated_at`. A
                    // timestamp captured just before either commit can produce
                    // a harmless `None`; prove the exact lease and replay with
                    // a fresh operational time. The same loop also recovers an
                    // ambiguous database response.
                    self.durable_turn_revision_retry(turn_id, lease_token)
                        .await?;
                    self.wait_for_durable_store_retry(turn_id).await?;
                }
            }
        }
    }

    async fn apply_durable_steer_retry(
        &self,
        turn_id: TurnId,
        lease_token: uuid::Uuid,
        steer_id: crate::id::TurnSteerId,
        attempt_event_ordinal: i32,
        preceding_assistant: Option<&Message>,
        preceding_citations: &[AssistantCitationReference],
    ) -> Result<JournaledTurnSteerOutcome> {
        let mut exact_retry_attempted = false;
        loop {
            match self
                .store
                .apply_turn_steer(
                    turn_id,
                    lease_token,
                    steer_id,
                    attempt_event_ordinal,
                    preceding_assistant,
                    preceding_citations,
                    Utc::now(),
                )
                .await
            {
                Ok(Some(applied)) => return Ok(applied),
                Ok(None) => {
                    self.durable_turn_revision_retry(turn_id, lease_token)
                        .await?;
                    self.wait_for_durable_store_retry(turn_id).await?;
                }
                Err(_) => {
                    // Retry the exact identity before classifying current turn
                    // state. A committed application remains recoverable after
                    // cancellation or lease expiry through its immutable
                    // receipt and journal identity.
                    if exact_retry_attempted {
                        self.wait_for_durable_store_retry(turn_id).await?;
                    } else {
                        exact_retry_attempted = true;
                        Delay::new(Duration::from_millis(10)).await;
                    }
                }
            }
        }
    }

    async fn wait_for_durable_store_retry(&self, turn_id: TurnId) -> Result<()> {
        match future::select(
            self.cancel.cancelled(),
            Delay::new(Duration::from_millis(10)),
        )
        .await
        {
            Either::Left(((), _)) => Err(AgentError::Store(format!(
                "turn {turn_id} was cancelled while retrying durable steering"
            ))),
            Either::Right(((), _)) => Ok(()),
        }
    }

    /// Resolve approval and execute one tool call, returning its output. Tool and
    /// approval failures surface as error output, never `Err`.
    async fn run_tool(
        &self,
        chat: &Chat,
        turn_id: TurnId,
        call: &PendingCall,
        events: &EventSink<'_>,
        durable_approval: Option<&crate::approval::ToolApproval>,
    ) -> ToolOutput {
        let Some(tool) = self.tools.get(&call.name) else {
            return ToolOutput::error(format!("unknown tool: {}", call.name));
        };
        // Policy: ReadOnly/Workspace auto; uncovered Sensitive calls park on the
        // approval gate.
        // Commit the approval request *before* emitting ApprovalRequired so a
        // client that sees the event can never race a 404 against a request
        // that exists only in this process.
        let approval_class = durable_approval
            .map(|approval| approval.class)
            .unwrap_or_else(|| tool.approval_class());
        // Standing grant: a brand-new Sensitive call the user already approved
        // for this chat runs without re-parking on the gate. A recovered call
        // (`durable_approval` present) keeps its durable park-and-resume
        // reconciliation. When `durable_approval` is `None` the request kind is
        // exactly `for_tool_name`, matching what the park block computes below.
        // Deny-by-default: only approvable kinds are ever covered.
        let bypass_by_standing_grant = matches!(approval_class, ApprovalClass::Sensitive)
            && durable_approval.is_none()
            && self.standing_grants.covers(
                chat.id,
                &call.name,
                ToolApprovalKind::for_tool_name(&call.name),
            );
        if matches!(approval_class, ApprovalClass::Sensitive) && !bypass_by_standing_grant {
            let summary = format!("{} requires approval", call.name);
            let kind = durable_approval
                .map(|approval| approval.kind)
                .unwrap_or_else(|| ToolApprovalKind::for_tool_name(&call.name));
            // A recovered call re-presents the preview durable state already
            // holds, so a reconnecting client sees the same command it was
            // asked about before the restart.
            let preview = match durable_approval {
                Some(approval) => approval.preview.clone(),
                None => call_action_preview(call),
            };
            if self.durable_steer_lease.is_some() && events.flush().await.is_err() {
                return ToolOutput::error("approval event journal is unavailable");
            }
            let journal = match (self.durable_steer_lease, events.proposed_ordinal()) {
                (_, Err(_)) => return ToolOutput::error("approval event journal is unavailable"),
                (Some(lease_token), Ok(Some(event_ordinal))) => Some(ApprovalJournalIdentity {
                    lease_token,
                    event_ordinal,
                }),
                (None, Ok(None)) => None,
                _ => return ToolOutput::error("approval event journal identity is invalid"),
            };
            let registering = self.approvals.register(
                ApprovalRequest {
                    call_id: call.call_id,
                    chat_id: chat.id,
                    turn_id,
                    tool_name: call.name.clone(),
                    class: ApprovalClass::Sensitive,
                    kind,
                    preview: preview.clone(),
                    summary: summary.clone(),
                },
                journal,
            );
            let registration = match future::select(registering, self.cancel.cancelled()).await {
                Either::Left((registration, _)) if !self.cancel.is_cancelled() => registration,
                Either::Left(_) | Either::Right(((), _)) => {
                    return ToolOutput::error("turn cancelled while registering approval");
                }
            };
            let required = AgentEvent::ApprovalRequired {
                call_id: call.call_id,
                tool_name: call.name.clone(),
                class: ApprovalClass::Sensitive,
                kind,
                preview,
                summary,
            };
            match registration.publication {
                ApprovalRequiredPublication::Ordinary => events.send(required),
                ApprovalRequiredPublication::Committed {
                    event_ordinal,
                    event,
                } => {
                    if events
                        .send_committed_proposed(event_ordinal, event)
                        .is_err()
                    {
                        return ToolOutput::error("approval event publication is unavailable");
                    }
                }
                ApprovalRequiredPublication::Recovered {
                    event_ordinal,
                    event,
                } => {
                    if events
                        .send_recovered_proposed(event_ordinal, event)
                        .is_err()
                    {
                        return ToolOutput::error("approval event recovery is unavailable");
                    }
                }
                ApprovalRequiredPublication::None => {}
            }
            let pending = registration.decision;
            // Race the decision against cancellation so a turn parked on approval
            // can still be stopped. On cancel we close the approval card
            // (`ApprovalDecided { approved: false }`) and return an error result;
            // the loop's post-tool check then ends the turn as cancelled.
            //
            // `future::select` polls the left arm first, so when both are ready
            // (approve lands in the same tick as cancel) the decision would win
            // and a Sensitive tool would still run. Prefer cancel whenever the
            // token is already tripped (same idea as the post-stream\n            // `is_cancelled()` re-check after `select`).
            let decision = match future::select(pending, self.cancel.cancelled()).await {
                Either::Left((decision, _)) if !self.cancel.is_cancelled() => decision,
                Either::Left(_) | Either::Right(((), _)) => {
                    events.send(AgentEvent::ApprovalDecided {
                        call_id: call.call_id,
                        approved: false,
                    });
                    return ToolOutput::error("turn cancelled while awaiting approval");
                }
            };
            let approved = matches!(decision, ApprovalDecision::Approve);
            events.send(AgentEvent::ApprovalDecided {
                call_id: call.call_id,
                approved,
            });
            if let ApprovalDecision::Reject { reason } = decision {
                return ToolOutput::error(reason);
            }
            // A cancel that lands after Approve won `select` but before execute
            // (concurrent trip of the token) must not run the Sensitive tool.
            if self.cancel.is_cancelled() {
                return ToolOutput::error("turn cancelled while awaiting approval");
            }
        }
        // Cancellation can land after the caller's loop-level fence or while a
        // recovered call is being classified. Recheck at the final boundary
        // before any ReadOnly, Workspace, or approved Sensitive implementation
        // can observe arguments or perform a side effect.
        if self.cancel.is_cancelled() {
            return ToolOutput::error("turn cancelled before tool execution");
        }
        let ctx = self
            .config
            .tool_scratch
            .as_ref()
            .map_or_else(
                || ToolCtx::without_private_scratch(chat.id, chat.project_id),
                |scratch| ToolCtx::with_private_scratch(chat.id, chat.project_id, scratch.clone()),
            )
            .with_call_id(call.call_id);
        // `future::select` polls cancellation first. If it wins, dropping the
        // unselected execution future propagates cancellation into async tools
        // such as reqwest instead of leaving egress alive after the turn ends.
        // Recheck after the execution arm wins to close a same-tick race.
        let executing = tool.execute(&ctx, parse_args(&call.args));
        let mut output = match future::select(self.cancel.cancelled(), executing).await {
            Either::Left(((), _)) => ToolOutput::error("turn cancelled during tool execution"),
            Either::Right((_, _)) if self.cancel.is_cancelled() => {
                ToolOutput::error("turn cancelled during tool execution")
            }
            Either::Right((result, _)) => match result {
                Ok(output) => output,
                Err(err) => ToolOutput::error(err.to_string()),
            },
        };
        if let Some(truncated) =
            truncate_to_bytes(&output.content, self.config.max_tool_result_bytes)
        {
            output.content = truncated;
        }
        output
    }

    /// Resume persisted server calls accepted by an earlier attempt before
    /// asking the provider for new output. Approval-bearing calls are isolated
    /// at admission, so their recovery never guesses batch order.
    async fn resume_pending_server_calls(
        &self,
        chat: &Chat,
        turn_id: TurnId,
        events: &EventSink<'_>,
        transcript: &mut Vec<ChatMessage>,
    ) -> Result<()> {
        let pending = self
            .store
            .list_tool_calls(chat.id)
            .await?
            .into_iter()
            .filter(|call| {
                call.turn_id == turn_id
                    && call.execution == ToolCallExecution::Server
                    && call.status == ToolCallStatus::Pending
            })
            .collect::<Vec<_>>();
        if pending.is_empty() {
            return Ok(());
        }
        let mut approval_bearing = 0usize;
        for call in &pending {
            if self.store.get_tool_call_approval(call.id).await?.is_some()
                || self
                    .tools
                    .get(&call.name)
                    .is_some_and(|tool| tool.approval_class() == ApprovalClass::Sensitive)
            {
                approval_bearing += 1;
            }
        }
        if approval_bearing > 0 && (pending.len() != 1 || approval_bearing != 1) {
            return Err(AgentError::Store(format!(
                "turn {turn_id} has an ambiguous pending sensitive tool batch"
            )));
        }
        for stored in pending {
            let call = PendingCall {
                call_id: stored.id,
                provider_id: stored.provider_id,
                name: stored.name,
                args: serde_json::to_string(&stored.arguments)?,
            };
            let durable_approval = self.store.get_tool_call_approval(call.call_id).await?;
            if self.durable_steer_lease.is_some() {
                // A pending call recovered at startup is ambiguous: the prior
                // process may have performed its side effect and died
                // before committing the result. Never execute it again. Commit
                // a deterministic failed result under this attempt's lease so
                // the model can recover without double-applying the effect.
                let output = ToolOutput::error(
                    "tool execution was interrupted before its result was committed; the call was not replayed",
                );
                let resolution = ToolCallResolution::Failed {
                    result: output.content.clone(),
                    error_code: "tool_execution_interrupted".into(),
                    error_detail: Some(
                        "a prior turn attempt may have executed this call; replay was suppressed"
                            .into(),
                    ),
                };
                let outcome = self
                    .abandon_inherited_server_call_retry(
                        chat.id,
                        turn_id,
                        call.call_id,
                        &resolution,
                    )
                    .await?;
                if !matches!(
                    outcome,
                    ResolveToolCallOutcome::Resolved | ResolveToolCallOutcome::Existing
                ) {
                    return Err(AgentError::Store(format!(
                        "inherited tool call {} could not be abandoned: {outcome:?}",
                        call.call_id
                    )));
                }
                if durable_approval.is_some() {
                    if let Some(approval) = self.store.get_tool_call_approval(call.call_id).await? {
                        events.send(AgentEvent::ApprovalDecided {
                            call_id: call.call_id,
                            approved: matches!(
                                approval.status,
                                crate::approval::ToolApprovalStatus::Approved
                            ),
                        });
                    }
                }
                events.send(AgentEvent::ToolCallCompleted {
                    call_id: call.call_id,
                    output: output.clone(),
                    action: call_action_preview(&call),
                    result: ToolResultPreview::build(&call.name, output.data.as_ref()),
                });
                transcript.push(ChatMessage {
                    role: Role::User,
                    content: vec![ContentBlock::ToolResult {
                        tool_use_id: call.provider_id,
                        content: output.content,
                        is_error: true,
                    }],
                });
                continue;
            }
            let tool_available = self.tools.get(&call.name).is_some();
            let cancelled_before_run = self.cancel.is_cancelled();
            let output = if cancelled_before_run {
                ToolOutput::error("turn cancelled before recovered tool execution")
            } else {
                self.run_tool(chat, turn_id, &call, events, durable_approval.as_ref())
                    .await
            };
            let resolution = if output.is_error {
                ToolCallResolution::Failed {
                    result: output.content.clone(),
                    error_code: "tool_error".into(),
                    error_detail: None,
                }
            } else {
                ToolCallResolution::Completed {
                    result: output.content.clone(),
                }
            };
            let outcome = self
                .store
                .resolve_server_tool_call_with_evidence(
                    call.call_id,
                    &resolution,
                    Utc::now(),
                    &output.private_evidence,
                )
                .await?;
            if !matches!(
                outcome,
                ResolveToolCallOutcome::Resolved | ResolveToolCallOutcome::Existing
            ) {
                return Err(AgentError::Store(format!(
                    "pending tool call {} could not be recovered: {outcome:?}",
                    call.call_id
                )));
            }
            // A missing implementation cannot enter `run_tool`'s approval
            // branch. Resolution above atomically closes any still-pending
            // approval with the failed call. Read back the winner so an
            // approve-vs-resolution race projects the authoritative decision.
            if durable_approval.is_some() && (!tool_available || cancelled_before_run) {
                if let Some(approval) = self.store.get_tool_call_approval(call.call_id).await? {
                    events.send(AgentEvent::ApprovalDecided {
                        call_id: call.call_id,
                        approved: matches!(
                            approval.status,
                            crate::approval::ToolApprovalStatus::Approved
                        ),
                    });
                }
            }
            events.send(AgentEvent::ToolCallCompleted {
                call_id: call.call_id,
                output: output.clone(),
                action: call_action_preview(&call),
                result: ToolResultPreview::build(&call.name, output.data.as_ref()),
            });
            transcript.push(ChatMessage {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: call.provider_id,
                    content: output.content,
                    is_error: output.is_error,
                }],
            });
        }
        Ok(())
    }

    async fn persist(
        &self,
        chat_id: crate::id::ChatId,
        turn_id: TurnId,
        role: Role,
        content: &str,
    ) -> Result<MessageId> {
        let id = MessageId::new();
        self.store
            .append_message(&Message {
                id,
                chat_id,
                turn_id,
                role,
                content: content.to_string(),
                created_at: Utc::now(),
            })
            .await?;
        Ok(id)
    }

    async fn persist_assistant(
        &self,
        chat_id: crate::id::ChatId,
        turn_id: TurnId,
        candidate: &AssistantCandidate,
    ) -> Result<MessageId> {
        let id = MessageId::new();
        let message = Message {
            id,
            chat_id,
            turn_id,
            role: Role::Assistant,
            content: candidate.content.clone(),
            created_at: Utc::now(),
        };
        self.append_assistant_exact_retry(&message, &candidate.citations)
            .await?;
        Ok(id)
    }

    async fn append_assistant_exact_retry(
        &self,
        message: &Message,
        citations: &[AssistantCitationReference],
    ) -> Result<()> {
        if let Some(lease_token) = self.durable_steer_lease {
            loop {
                match self
                    .store
                    .append_claimed_assistant_message_with_citations(
                        message,
                        citations,
                        lease_token,
                        Utc::now(),
                    )
                    .await
                {
                    Ok(AppendClaimedMessageOutcome::Appended)
                    | Ok(AppendClaimedMessageOutcome::Existing) => return Ok(()),
                    Ok(AppendClaimedMessageOutcome::IdentityConflict) => {
                        return Err(AgentError::Store(format!(
                            "message identity {} conflicts with its claimed assistant payload",
                            message.id
                        )));
                    }
                    Ok(AppendClaimedMessageOutcome::LeaseLost) => {
                        return Err(AgentError::Store(format!(
                            "turn {} lost its lease while appending assistant message {}",
                            message.turn_id, message.id
                        )));
                    }
                    Err(_) => {
                        self.ensure_durable_lease_current(message.turn_id).await?;
                        self.wait_for_durable_store_retry(message.turn_id).await?;
                    }
                }
            }
        }
        if self
            .store
            .append_assistant_message_with_citations(message, citations)
            .await
            .is_err()
        {
            // The first response can be lost after commit. Reuse every stable
            // request field so storage can prove and recover only that exact
            // message/citation sequence.
            self.store
                .append_assistant_message_with_citations(message, citations)
                .await?;
        }
        Ok(())
    }

    async fn load_transcript(&self, chat_id: crate::id::ChatId) -> Result<Vec<ChatMessage>> {
        let messages = self.store.list_messages(chat_id).await?;
        let tool_calls = self.store.list_tool_calls(chat_id).await?;
        Ok(rebuild_transcript(&messages, &tool_calls))
    }

    /// Fit the transcript to the context budget at the given reduction level.
    /// Returns the fitted transcript and whether it was shortened.
    fn fit_transcript(
        &self,
        transcript: &[ChatMessage],
        reduction_level: u32,
    ) -> (Vec<ChatMessage>, bool) {
        let budget = context::compute_message_budget(
            self.config.context_window,
            reduction_level,
            self.config.system_prompt.as_deref(),
            &self
                .tools
                .specs_for_foreground(self.agent_orchestration_active()),
        );
        let floor = context::content_floor_for_level(reduction_level);
        context::fit_to_budget(transcript, budget, floor)
    }
}

/// Merge text messages and structured tool-call rows into the provider transcript.
///
/// Tool calls are partitioned into *batches*: a new batch starts when a call's
/// `created_at` is at or after the previous batch's latest `resolved_at`. That
/// matches the agent loop (upsert all args for a model step, then complete them,
/// then the next model step). Batches that fall after an assistant text message
/// and before the next message attach as `ToolUse` on that assistant; otherwise
/// they become a tool-only assistant step. `ToolResult` blocks follow as a user
/// message. Legacy `Role::Tool` rows are ignored.
fn rebuild_transcript(messages: &[Message], tool_calls: &[ToolCallRecord]) -> Vec<ChatMessage> {
    let messages: Vec<&Message> = messages
        .iter()
        .filter(|message| message.role != Role::Tool)
        .collect();
    let batches = batch_tool_calls(tool_calls);
    let mut batch_i = 0;
    let mut out: Vec<ChatMessage> = Vec::new();

    for (i, message) in messages.iter().enumerate() {
        // Batches that started before this message are prior tool-only steps.
        while batch_i < batches.len() && batches[batch_i][0].created_at < message.created_at {
            push_tool_batch(&mut out, &batches[batch_i], None);
            batch_i += 1;
        }

        if message.role == Role::Assistant {
            let next_ts = messages.get(i + 1).map(|m| m.created_at);
            let text = if message.content.is_empty() {
                None
            } else {
                Some(message.content.as_str())
            };
            // Same model step: tools upserted right after the assistant text.
            if batch_i < batches.len()
                && next_ts.is_none_or(|end| batches[batch_i][0].created_at < end)
            {
                push_tool_batch(&mut out, &batches[batch_i], text);
                batch_i += 1;
                while batch_i < batches.len()
                    && next_ts.is_none_or(|end| batches[batch_i][0].created_at < end)
                {
                    push_tool_batch(&mut out, &batches[batch_i], None);
                    batch_i += 1;
                }
            } else if let Some(text) = text {
                out.push(ChatMessage::text(Role::Assistant, text.to_string()));
            }
        } else {
            out.push(ChatMessage::text(message.role, message.content.clone()));
            // Tool-only steps between this message and the next non-assistant
            // (e.g. user → tools → user steer). If the next message is
            // assistant, that branch claims the batch instead.
            let next_ts = messages.get(i + 1).map(|m| m.created_at);
            let next_is_assistant = messages
                .get(i + 1)
                .is_some_and(|m| m.role == Role::Assistant);
            if !next_is_assistant {
                while batch_i < batches.len()
                    && next_ts.is_none_or(|end| batches[batch_i][0].created_at < end)
                {
                    push_tool_batch(&mut out, &batches[batch_i], None);
                    batch_i += 1;
                }
            }
        }
    }

    while batch_i < batches.len() {
        push_tool_batch(&mut out, &batches[batch_i], None);
        batch_i += 1;
    }

    out
}

#[cfg(test)]
pub(crate) fn rebuild_transcript_for_test(
    messages: &[Message],
    tool_calls: &[ToolCallRecord],
) -> Vec<ChatMessage> {
    rebuild_transcript(messages, tool_calls)
}

/// Partition calls into per-model-step batches (see [`rebuild_transcript`]).
fn batch_tool_calls(tool_calls: &[ToolCallRecord]) -> Vec<Vec<&ToolCallRecord>> {
    let mut batches: Vec<Vec<&ToolCallRecord>> = Vec::new();
    let mut current: Vec<&ToolCallRecord> = Vec::new();
    let mut batch_done_at: Option<chrono::DateTime<Utc>> = None;

    for call in tool_calls {
        if call.execution == ToolCallExecution::Orchestration {
            if !current.is_empty() {
                batches.push(std::mem::take(&mut current));
            }
            batches.push(vec![call]);
            batch_done_at = None;
            continue;
        }
        if let Some(done) = batch_done_at {
            if call.created_at >= done {
                batches.push(std::mem::take(&mut current));
                batch_done_at = None;
            }
        }
        current.push(call);
        if let Some(completed) = call.resolved_at {
            batch_done_at = Some(match batch_done_at {
                Some(done) => done.max(completed),
                None => completed,
            });
        }
    }
    if !current.is_empty() {
        batches.push(current);
    }
    batches
}

fn push_tool_batch(
    out: &mut Vec<ChatMessage>,
    batch: &[&ToolCallRecord],
    assistant_text: Option<&str>,
) {
    let mut blocks: Vec<ContentBlock> = Vec::new();
    if let Some(text) = assistant_text.filter(|t| !t.is_empty()) {
        blocks.push(ContentBlock::Text {
            text: text.to_string(),
        });
    }
    for call in batch {
        blocks.push(ContentBlock::ToolUse {
            id: call.provider_id.clone(),
            name: call.name.clone(),
            input: call.arguments.clone(),
        });
    }
    if !blocks.is_empty() {
        out.push(ChatMessage {
            role: Role::Assistant,
            content: blocks,
        });
    }
    let results: Vec<ContentBlock> = batch
        .iter()
        .filter_map(|call| {
            call.result
                .as_ref()
                .map(|content| ContentBlock::ToolResult {
                    tool_use_id: call.provider_id.clone(),
                    content: content.clone(),
                    is_error: call.status != ToolCallStatus::Completed,
                })
        })
        .collect();
    if !results.is_empty() {
        out.push(ChatMessage {
            role: Role::User,
            content: results,
        });
    }
}

/// Truncate `content` to at most `max_bytes` (on a UTF-8 char boundary) and
/// append a notice. Returns `None` when it already fits.
fn truncate_to_bytes(content: &str, max_bytes: usize) -> Option<String> {
    if content.len() <= max_bytes {
        return None;
    }
    let mut end = max_bytes;
    while end > 0 && !content.is_char_boundary(end) {
        end -= 1;
    }
    Some(format!(
        "{}\n\n[truncated: {} of {} bytes shown]",
        &content[..end],
        end,
        content.len()
    ))
}

/// Parse accumulated tool-call args; malformed JSON becomes an empty object so a
/// tool can report the problem itself rather than aborting the turn.
fn parse_args(raw: &str) -> Value {
    if raw.trim().is_empty() {
        return Value::Object(Default::default());
    }
    serde_json::from_str(raw).unwrap_or(Value::Object(Default::default()))
}

/// Client-owned calls cross a trusted execution boundary, so malformed input
/// must be retried by the model rather than silently changed before dispatch.
fn parse_client_args(raw: &str) -> Option<Value> {
    if raw.trim().is_empty() {
        return Some(Value::Object(Default::default()));
    }
    serde_json::from_str(raw).ok()
}

// The end-to-end test needs the SQLite store and the built-in tools.
#[cfg(all(test, feature = "sqlite", feature = "tools"))]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Mutex;

    use async_trait::async_trait;
    use chrono::DateTime;
    use futures::channel::mpsc::unbounded;
    use futures::stream::{self, BoxStream};

    use super::*;
    use crate::db::DbStore;
    use crate::id::{ChatId, ProjectId};
    use crate::model::Project;
    use crate::provider::ProviderId;
    use crate::tools::{ReadFile, WriteFile};

    fn tool_scratch(path: &std::path::Path) -> ToolScratch {
        ToolScratch::from_dir(
            cap_std::fs::Dir::open_ambient_dir(path, cap_std::ambient_authority()).unwrap(),
        )
    }

    fn emitted_events(emissions: Vec<ClaimedAgentEvent>) -> Vec<AgentEvent> {
        emissions
            .into_iter()
            .map(|emission| match emission {
                ClaimedAgentEvent::Pending { event, .. } => event,
                ClaimedAgentEvent::Committed { event, .. } => event.event,
                ClaimedAgentEvent::Recovered { event, .. } => event.event,
                ClaimedAgentEvent::Flush(_) => panic!("unhandled claimed-event flush"),
            })
            .collect()
    }

    fn streamed_text(events: &[AgentEvent]) -> String {
        events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::TextDelta { text } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn stream_filter_preserves_normal_and_cancel_tails_but_discards_steered_tail() {
        let incomplete = "normal [[ow-source:abcdef";
        let (normal_tx, normal_rx) = unbounded();
        let normal_sink = EventSink::Legacy(&normal_tx);
        let mut normal = AssistantStreamEventFilter::new(&normal_sink);
        for character in incomplete.chars() {
            normal.send_text(&character.to_string());
        }
        normal.finish();
        drop(normal);
        drop(normal_tx);
        let normal_events = normal_rx.collect::<Vec<_>>().await;
        assert_eq!(streamed_text(&normal_events), incomplete);

        let malformed = "cancel [[ow-source:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA]]";
        let (cancel_tx, cancel_rx) = unbounded();
        let cancel_sink = EventSink::Legacy(&cancel_tx);
        let mut cancelled = AssistantStreamEventFilter::new(&cancel_sink);
        for character in malformed.chars() {
            cancelled.send_text(&character.to_string());
        }
        // Cancellation uses the same literal flush as a normal stream end
        // before the terminal cancellation event is published.
        cancelled.finish();
        cancelled.send(AgentEvent::TurnCancelled {
            usage: Usage::default(),
        });
        drop(cancelled);
        drop(cancel_tx);
        let cancel_events = cancel_rx.collect::<Vec<_>>().await;
        assert_eq!(streamed_text(&cancel_events), malformed);
        assert!(matches!(
            cancel_events.last(),
            Some(AgentEvent::TurnCancelled { .. })
        ));

        let (steer_tx, steer_rx) = unbounded();
        let steer_sink = EventSink::Legacy(&steer_tx);
        let mut steered = AssistantStreamEventFilter::new(&steer_sink);
        steered.send_text("steer ");
        steered.send_text("[[ow-source:abcdef");
        steered.discard();
        steered.send(AgentEvent::StreamInterrupted);
        drop(steered);
        drop(steer_tx);
        let steer_events = steer_rx.collect::<Vec<_>>().await;
        assert_eq!(streamed_text(&steer_events), "steer ");
        assert!(matches!(
            steer_events.last(),
            Some(AgentEvent::StreamInterrupted)
        ));
    }

    #[test]
    fn client_tool_arguments_are_parsed_without_forgiving_malformed_json() {
        assert_eq!(
            parse_client_args(""),
            Some(Value::Object(Default::default()))
        );
        assert_eq!(
            parse_client_args(r#"{"hint":"Documents"}"#),
            Some(serde_json::json!({"hint": "Documents"}))
        );
        assert_eq!(parse_client_args(r#"{"hint":"Documents""#), None);
    }

    /// A scripted provider: step 0 calls `read_file`, step 1 gives a final answer.
    struct FakeProvider {
        calls: AtomicUsize,
    }

    struct ClientToolProvider {
        assistant_text: bool,
        /// Emit a second, server-executed call beside the client one — the
        /// shape the loop still refuses, since a checkpoint carries one call.
        sibling_call: bool,
        name: &'static str,
        arguments: &'static str,
    }

    struct SandboxCorrectionProvider {
        calls: AtomicUsize,
    }

    struct ContextRecordingTool {
        observed_project: Arc<Mutex<Option<Option<ProjectId>>>>,
        observed_call: Arc<Mutex<Option<CallId>>>,
    }

    struct CitationSearchTool {
        source_token: uuid::Uuid,
    }

    #[async_trait]
    impl Tool for CitationSearchTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "search".into(),
                description: "test search".into(),
                input_schema: serde_json::json!({"type": "object"}),
            }
        }

        fn approval_class(&self) -> ApprovalClass {
            ApprovalClass::ReadOnly
        }

        async fn execute(&self, _ctx: &ToolCtx, _args: Value) -> Result<ToolOutput> {
            let document_id = crate::DocumentId::new();
            let span = crate::ByteSpan::new(0, 8);
            Ok(
                ToolOutput::text("search result").with_private_evidence(vec![
                    crate::RetrievalEvidenceInput {
                        rank: 1,
                        source_token: self.source_token,
                        document_id,
                        generation: crate::DocumentGeneration {
                            content_revision: 1,
                            revision_token: uuid::Uuid::new_v4(),
                        },
                        chunk_id: crate::ChunkId::derive(document_id, span.start, span.end),
                        span,
                        snippet: "evidence".into(),
                        heading_path: vec!["Facts".into()],
                        source_regions: Vec::new(),
                        source: crate::RetrievalEvidenceSource::Inline,
                    },
                ]),
            )
        }
    }

    struct IntermediateCitationProvider {
        calls: AtomicUsize,
        marker: String,
    }

    #[async_trait]
    impl ModelProvider for IntermediateCitationProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("citation-test")
        }

        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            let step = self.calls.fetch_add(1, Ordering::SeqCst);
            let events = match step {
                0 => vec![
                    ProviderEvent::ToolCallStarted {
                        index: 0,
                        id: "search_1".into(),
                        name: "search".into(),
                    },
                    ProviderEvent::ToolCallArgsDelta {
                        index: 0,
                        fragment: "{}".into(),
                    },
                    ProviderEvent::Stop {
                        reason: StopReason::ToolUse,
                    },
                ],
                1 => {
                    let candidate = format!("intermediate {}", self.marker);
                    let mut events = candidate
                        .chars()
                        .map(|character| ProviderEvent::TextDelta {
                            text: character.to_string(),
                        })
                        .collect::<Vec<_>>();
                    events.extend([
                        ProviderEvent::ToolCallStarted {
                            index: 0,
                            id: "read_1".into(),
                            name: "read_file".into(),
                        },
                        ProviderEvent::ToolCallArgsDelta {
                            index: 0,
                            fragment: r#"{"path":"note.txt"}"#.into(),
                        },
                        ProviderEvent::Stop {
                            reason: StopReason::ToolUse,
                        },
                    ]);
                    events
                }
                _ => vec![
                    ProviderEvent::TextDelta {
                        text: "final".into(),
                    },
                    ProviderEvent::Stop {
                        reason: StopReason::EndTurn,
                    },
                ],
            };
            Ok(stream::iter(events).boxed())
        }
    }

    #[tokio::test]
    async fn intermediate_assistant_source_marker_is_stripped_and_attached_atomically() {
        let db = tempfile::tempdir().unwrap();
        let store = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                db.path().join("citations.db").display()
            ))
            .await
            .unwrap(),
        );
        let chat = Chat {
            id: ChatId::new(),
            project_id: None,
            title: None,
            model: None,
            reasoning_effort: None,
            attachment_revision: 0,
            root_attachments: Vec::new(),
            created_at: Utc::now(),
        };
        store.create_chat(&chat).await.unwrap();
        let source_token = uuid::Uuid::new_v4();
        let marker =
            crate::format_source_reference(crate::AssistantCitationReference { source_token });
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("note.txt"), "note").unwrap();
        let agent = Agent::new(
            Arc::new(IntermediateCitationProvider {
                calls: AtomicUsize::new(0),
                marker,
            }),
            Arc::new(
                ToolRegistry::new()
                    .with(Box::new(CitationSearchTool { source_token }))
                    .with(Box::new(ReadFile)),
            ),
            store.clone(),
            AgentConfig {
                model: "test".into(),
                tool_scratch: Some(tool_scratch(workspace.path())),
                ..Default::default()
            },
        );
        let (tx, _rx) = unbounded();
        agent.run_turn(&chat, "question", &tx).await.unwrap();
        let transcript = store.get_chat_transcript(chat.id).await.unwrap().unwrap();
        assert!(transcript
            .messages
            .iter()
            .all(|message| !message.content.contains("[[ow-source:")));
        let intermediate = transcript
            .messages
            .iter()
            .find(|message| message.content == "intermediate ")
            .expect("clean intermediate assistant message");
        assert_eq!(transcript.citations.len(), 1);
        assert_eq!(transcript.citations[0].message_id, intermediate.id);
    }

    #[async_trait]
    impl Tool for ContextRecordingTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "read_file".into(),
                description: "record invocation context".into(),
                input_schema: serde_json::json!({"type": "object"}),
            }
        }

        fn approval_class(&self) -> ApprovalClass {
            ApprovalClass::ReadOnly
        }

        async fn execute(&self, ctx: &ToolCtx, _args: Value) -> Result<ToolOutput> {
            *self.observed_project.lock().unwrap() = Some(ctx.project_id);
            *self.observed_call.lock().unwrap() = ctx.call_id;
            Ok(ToolOutput::text("recorded"))
        }
    }

    #[async_trait]
    impl ModelProvider for FakeProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("fake")
        }
        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            let events = if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                vec![
                    ProviderEvent::ToolCallStarted {
                        index: 0,
                        id: "call_1".into(),
                        name: "read_file".into(),
                    },
                    ProviderEvent::ToolCallArgsDelta {
                        index: 0,
                        fragment: r#"{"path":"note.txt"}"#.into(),
                    },
                    ProviderEvent::Usage(Usage {
                        input_tokens: 5,
                        output_tokens: 2,
                        ..Default::default()
                    }),
                    ProviderEvent::Stop {
                        reason: StopReason::ToolUse,
                    },
                ]
            } else {
                vec![
                    ProviderEvent::TextDelta {
                        text: "done".into(),
                    },
                    ProviderEvent::Usage(Usage {
                        input_tokens: 3,
                        output_tokens: 4,
                        ..Default::default()
                    }),
                    ProviderEvent::Stop {
                        reason: StopReason::EndTurn,
                    },
                ]
            };
            Ok(stream::iter(events).boxed())
        }
    }

    #[async_trait]
    impl ModelProvider for ClientToolProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("client-tool")
        }

        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            let mut events = vec![
                ProviderEvent::ToolCallStarted {
                    index: 0,
                    id: "native_1".into(),
                    name: self.name.into(),
                },
                ProviderEvent::ToolCallArgsDelta {
                    index: 0,
                    fragment: self.arguments.into(),
                },
                ProviderEvent::Usage(Usage {
                    input_tokens: 5,
                    output_tokens: 2,
                    ..Usage::default()
                }),
                ProviderEvent::Stop {
                    reason: StopReason::ToolUse,
                },
            ];
            if self.sibling_call {
                events.splice(
                    1..1,
                    [
                        ProviderEvent::ToolCallStarted {
                            index: 1,
                            id: "native_2".into(),
                            name: "read_file".into(),
                        },
                        ProviderEvent::ToolCallArgsDelta {
                            index: 1,
                            fragment: r#"{"path":"a.txt"}"#.into(),
                        },
                    ],
                );
            }
            if self.assistant_text {
                events.insert(
                    0,
                    ProviderEvent::TextDelta {
                        text: "I will connect it".into(),
                    },
                );
            }
            Ok(stream::iter(events).boxed())
        }
    }

    #[async_trait]
    impl ModelProvider for SandboxCorrectionProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("sandbox-correction")
        }

        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            let first = self.calls.fetch_add(1, Ordering::SeqCst) == 0;
            let arguments = if first {
                r#"{"task":"Research the error handling options.","resource":null}"#
            } else {
                r#"{"task":"Research the error handling options."}"#
            };
            Ok(stream::iter(vec![
                ProviderEvent::ToolCallStarted {
                    index: 0,
                    id: if first {
                        "sandbox_null".into()
                    } else {
                        "sandbox_omitted".into()
                    },
                    name: crate::SPAWN_SANDBOX_AGENT_TOOL.into(),
                },
                ProviderEvent::ToolCallArgsDelta {
                    index: 0,
                    fragment: arguments.into(),
                },
                ProviderEvent::Usage(Usage {
                    input_tokens: 5,
                    output_tokens: 2,
                    ..Usage::default()
                }),
                ProviderEvent::Stop {
                    reason: StopReason::ToolUse,
                },
            ])
            .boxed())
        }
    }

    #[tokio::test]
    async fn claimed_agent_returns_a_client_tool_checkpoint_without_executing_it() {
        let db = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                db.path().join("t.db").display()
            ))
            .await
            .unwrap(),
        );
        let chat = Chat {
            id: ChatId::new(),
            project_id: None,
            title: None,
            model: None,
            reasoning_effort: None,
            attachment_revision: 0,
            root_attachments: Vec::new(),
            created_at: Utc::now(),
        };
        store.create_chat(&chat).await.unwrap();
        let turn_id = TurnId::new();
        store
            .accept_turn(turn_id, chat.id, "fake", "connect documents")
            .await
            .unwrap();
        let lease_token = uuid::Uuid::new_v4();
        let claimed_at = Utc::now();
        store
            .claim_turn_run(
                lease_token,
                claimed_at,
                claimed_at + chrono::Duration::minutes(1),
            )
            .await
            .unwrap();
        let client_spec = ToolSpec {
            name: "connect_folder".into(),
            description: "Ask the desktop to connect a folder".into(),
            input_schema: serde_json::json!({"type": "object"}),
        };
        let mut registry = ToolRegistry::new();
        registry.register_client(client_spec.clone());
        assert_eq!(
            registry.execution("connect_folder"),
            Some(ToolCallExecution::Client)
        );
        assert!(registry.get("connect_folder").is_none());
        assert_eq!(registry.specs(), vec![client_spec]);
        let agent = Agent::new(
            Arc::new(ClientToolProvider {
                assistant_text: false,
                sibling_call: false,
                name: "connect_folder",
                arguments: r#"{"hint":"Documents"}"#,
            }),
            Arc::new(registry),
            store.clone(),
            AgentConfig {
                model: "fake".into(),
                ..AgentConfig::default()
            },
        )
        .with_durable_steer(lease_token);
        let (tx, rx) = unbounded();
        let outcome = agent
            .run_claimed_turn(&chat, turn_id, MessageId::new(), 1, &tx)
            .await
            .unwrap();
        drop(tx);
        let events = emitted_events(rx.collect().await);
        let AgentTurnOutcome::ClientToolCall {
            request,
            usage,
            steer_revision,
            model_steps,
        } = outcome
        else {
            panic!("claimed agent should return a client checkpoint");
        };
        assert_eq!(request.chat_id, chat.id);
        assert_eq!(request.turn_id, turn_id);
        assert_eq!(request.provider_id, "native_1");
        assert_eq!(request.name, "connect_folder");
        assert_eq!(request.arguments, serde_json::json!({"hint": "Documents"}));
        assert_eq!(usage.input_tokens, 5);
        assert_eq!(usage.output_tokens, 2);
        assert_eq!(steer_revision, 0);
        assert_eq!(model_steps, 1);
        assert!(store.list_tool_calls(chat.id).await.unwrap().is_empty());
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::ToolCallStarted { name, .. } if name == "connect_folder"
        )));

        let mut validated_registry = ToolRegistry::new();
        validated_registry.register_validated_client(
            crate::request_folder_access_tool_spec(),
            crate::validate_request_folder_access_arguments,
        );
        let invalid_agent = Agent::new(
            Arc::new(ClientToolProvider {
                assistant_text: false,
                sibling_call: false,
                name: crate::REQUEST_FOLDER_ACCESS_TOOL,
                arguments: r#"{"reason":"Read reports","requested_capabilities":["write_files"],"path":"/Users/example/Documents"}"#,
            }),
            Arc::new(validated_registry),
            store.clone(),
            AgentConfig {
                model: "fake".into(),
                max_steps: 1,
                ..AgentConfig::default()
            },
        )
        .with_durable_steer(lease_token);
        let (invalid_tx, _invalid_rx) = unbounded();
        let invalid_outcome = invalid_agent
            .run_claimed_turn(&chat, turn_id, MessageId::new(), 1, &invalid_tx)
            .await
            .unwrap();
        assert!(matches!(
            invalid_outcome,
            AgentTurnOutcome::Failed {
                error,
                model_steps: 1,
                ..
            } if error.kind == "max_steps_exceeded"
        ));
        assert!(store.list_tool_calls(chat.id).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn user_questions_are_advertised_and_executable_only_in_the_foreground() {
        let mut registry = ToolRegistry::new();
        registry.register_validated_foreground_client(
            crate::ask_user_questions_tool_spec(),
            crate::validate_ask_user_questions_arguments,
        );

        assert!(registry.specs().is_empty());
        assert_eq!(
            registry
                .specs_for_foreground(true)
                .into_iter()
                .map(|spec| spec.name)
                .collect::<Vec<_>>(),
            vec![crate::ASK_USER_QUESTIONS_TOOL]
        );
        assert_eq!(
            registry.execution(crate::ASK_USER_QUESTIONS_TOOL),
            Some(ToolCallExecution::Client)
        );
        assert!(registry.is_foreground_client(crate::ASK_USER_QUESTIONS_TOOL));
        assert!(registry.client_arguments_are_valid(
            crate::ASK_USER_QUESTIONS_TOOL,
            &serde_json::json!({
                "questions": [{
                    "id": "target",
                    "header": "Target",
                    "question": "Where should I deploy?",
                    "options": [{
                        "id": "staging",
                        "label": "Staging",
                        "description": "Deploy for verification."
                    }]
                }]
            })
        ));
        assert!(!registry.client_arguments_are_valid(
            crate::ASK_USER_QUESTIONS_TOOL,
            &serde_json::json!({"questions": []})
        ));

        let db = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                db.path().join("foreground-question.db").display()
            ))
            .await
            .unwrap(),
        );
        let chat = Chat {
            id: ChatId::new(),
            project_id: None,
            title: None,
            model: None,
            reasoning_effort: None,
            attachment_revision: 0,
            root_attachments: Vec::new(),
            created_at: Utc::now(),
        };
        store.create_chat(&chat).await.unwrap();
        let agent = Agent::new(
            Arc::new(ClientToolProvider {
                assistant_text: false,
                sibling_call: false,
                name: crate::ASK_USER_QUESTIONS_TOOL,
                arguments: r#"{"questions":[{"id":"target","header":"Target","question":"Where should I deploy?","options":[{"id":"staging","label":"Staging","description":"Deploy for verification."}]}]}"#,
            }),
            Arc::new(registry),
            store.clone(),
            AgentConfig {
                model: "fake".into(),
                max_steps: 1,
                ..AgentConfig::default()
            },
        );
        let (tx, rx) = unbounded();
        agent.run_turn(&chat, "deploy", &tx).await.unwrap();
        drop(tx);
        let events = rx.collect::<Vec<_>>().await;
        assert!(events
            .iter()
            .any(|event| matches!(event, AgentEvent::TurnFailed { .. })));
        assert!(!events
            .iter()
            .any(|event| matches!(event, AgentEvent::UserQuestionsAsked { .. })));
        assert!(store.list_tool_calls(chat.id).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn claimed_foreground_agent_returns_one_bounded_sandbox_checkpoint() {
        let db = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                db.path().join("t.db").display()
            ))
            .await
            .unwrap(),
        );
        let chat = Chat {
            id: ChatId::new(),
            project_id: None,
            title: None,
            model: None,
            reasoning_effort: None,
            attachment_revision: 0,
            root_attachments: Vec::new(),
            created_at: Utc::now(),
        };
        store.create_chat(&chat).await.unwrap();
        let turn_id = TurnId::new();
        store
            .accept_turn(turn_id, chat.id, "fake", "research this")
            .await
            .unwrap();
        let lease_token = uuid::Uuid::new_v4();
        let claimed_at = Utc::now();
        store
            .claim_turn_run(
                lease_token,
                claimed_at,
                claimed_at + chrono::Duration::minutes(1),
            )
            .await
            .unwrap();

        let mut registry = ToolRegistry::new();
        registry.register_foreground_agent_orchestration();
        assert!(registry.specs().is_empty());
        let advertised = registry
            .specs_for_foreground(true)
            .into_iter()
            .map(|spec| spec.name)
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(
            advertised,
            [crate::SPAWN_SANDBOX_AGENT_TOOL, crate::WAIT_FOR_AGENTS_TOOL]
                .into_iter()
                .map(str::to_owned)
                .collect()
        );
        let agent = Agent::new(
            Arc::new(ClientToolProvider {
                assistant_text: false,
                sibling_call: false,
                name: crate::SPAWN_SANDBOX_AGENT_TOOL,
                arguments: r#"{"task":"Research the error handling options."}"#,
            }),
            Arc::new(registry),
            store.clone(),
            AgentConfig {
                model: "fake".into(),
                ..AgentConfig::default()
            },
        )
        .with_durable_steer(lease_token)
        .with_foreground_agent_orchestration();
        let (tx, rx) = unbounded();
        let outcome = agent
            .run_claimed_turn(&chat, turn_id, MessageId::new(), 1, &tx)
            .await
            .unwrap();
        drop(tx);
        let events = emitted_events(rx.collect().await);
        let AgentTurnOutcome::SandboxAgentSpawn {
            request,
            usage,
            steer_revision,
            model_steps,
        } = outcome
        else {
            panic!("foreground agent should return a sandbox checkpoint");
        };
        assert_eq!(request.task, "Research the error handling options.");
        assert_eq!(
            request.child_run_id,
            AgentRunId::sandbox_for_spawn_call(request.call_id)
        );
        assert!(request.is_well_formed());
        assert_eq!(usage.input_tokens, 5);
        assert_eq!(usage.output_tokens, 2);
        assert_eq!(steer_revision, 0);
        assert_eq!(model_steps, 1);
        assert!(store.list_tool_calls(chat.id).await.unwrap().is_empty());
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::ToolCallStarted { name, .. }
                if name == crate::SPAWN_SANDBOX_AGENT_TOOL
        )));

        let mut correction_registry = ToolRegistry::new();
        correction_registry.register_foreground_agent_orchestration();
        let correction_provider = Arc::new(SandboxCorrectionProvider {
            calls: AtomicUsize::new(0),
        });
        let correction_agent = Agent::new(
            correction_provider.clone(),
            Arc::new(correction_registry),
            store.clone(),
            AgentConfig {
                model: "fake".into(),
                ..AgentConfig::default()
            },
        )
        .with_durable_steer(lease_token)
        .with_foreground_agent_orchestration();
        let (correction_tx, correction_rx) = unbounded();
        let corrected = correction_agent
            .run_claimed_turn(&chat, turn_id, MessageId::new(), 1, &correction_tx)
            .await
            .unwrap();
        drop(correction_tx);
        let correction_events = emitted_events(correction_rx.collect().await);
        let AgentTurnOutcome::SandboxAgentSpawn {
            request,
            model_steps,
            ..
        } = corrected
        else {
            panic!("foreground agent should correct a noncanonical sandbox resource");
        };
        assert_eq!(correction_provider.calls.load(Ordering::SeqCst), 2);
        assert_eq!(model_steps, 2);
        assert_eq!(
            request.arguments,
            serde_json::json!({"task": "Research the error handling options."})
        );
        assert!(request.is_well_formed());
        assert!(store.list_tool_calls(chat.id).await.unwrap().is_empty());
        assert!(correction_events
            .iter()
            .any(|event| matches!(event, AgentEvent::StreamInterrupted)));
    }

    #[tokio::test]
    async fn claimed_foreground_agent_returns_exact_ordered_wait_checkpoint() {
        let db = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                db.path().join("wait.db").display()
            ))
            .await
            .unwrap(),
        );
        let chat = Chat {
            id: ChatId::new(),
            project_id: None,
            title: None,
            model: None,
            reasoning_effort: None,
            attachment_revision: 0,
            root_attachments: Vec::new(),
            created_at: Utc::now(),
        };
        store.create_chat(&chat).await.unwrap();
        let turn_id = TurnId::new();
        store
            .accept_turn(turn_id, chat.id, "fake", "wait for both")
            .await
            .unwrap();
        let lease_token = uuid::Uuid::new_v4();
        let claimed_at = Utc::now();
        store
            .claim_turn_run(
                lease_token,
                claimed_at,
                claimed_at + chrono::Duration::minutes(1),
            )
            .await
            .unwrap();
        let mut registry = ToolRegistry::new();
        registry.register_foreground_agent_orchestration();
        let arguments = r#"{"agent_ids":["00000000-0000-0000-0000-000000000002","00000000-0000-0000-0000-000000000001"]}"#;
        let agent = Agent::new(
            Arc::new(ClientToolProvider {
                assistant_text: false,
                sibling_call: false,
                name: crate::WAIT_FOR_AGENTS_TOOL,
                arguments,
            }),
            Arc::new(registry),
            store,
            AgentConfig {
                model: "fake".into(),
                ..AgentConfig::default()
            },
        )
        .with_durable_steer(lease_token)
        .with_foreground_agent_orchestration();
        let (tx, _rx) = unbounded();
        let outcome = agent
            .run_claimed_turn(&chat, turn_id, MessageId::new(), 1, &tx)
            .await
            .unwrap();
        let AgentTurnOutcome::WaitForAgents {
            request,
            steer_revision,
            model_steps,
            ..
        } = outcome
        else {
            panic!("foreground agent should return an ordered wait checkpoint");
        };
        assert_eq!(request.provider_id, "native_1");
        assert_eq!(
            request.arguments,
            serde_json::from_str::<Value>(arguments).unwrap()
        );
        assert_eq!(
            request.child_run_ids,
            [
                "00000000-0000-0000-0000-000000000002",
                "00000000-0000-0000-0000-000000000001",
            ]
            .map(|id| AgentRunId(uuid::Uuid::parse_str(id).unwrap()))
        );
        assert!(request.is_well_formed());
        assert_eq!(steer_revision, 0);
        assert_eq!(model_steps, 1);
    }

    #[tokio::test]
    async fn claimed_agent_retries_a_client_call_with_siblings_then_preserves_exhausted_usage() {
        let db = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                db.path().join("t.db").display()
            ))
            .await
            .unwrap(),
        );
        let chat = Chat {
            id: ChatId::new(),
            project_id: None,
            title: None,
            model: None,
            reasoning_effort: None,
            attachment_revision: 0,
            root_attachments: Vec::new(),
            created_at: Utc::now(),
        };
        store.create_chat(&chat).await.unwrap();
        let turn_id = TurnId::new();
        store
            .accept_turn(turn_id, chat.id, "fake", "connect documents")
            .await
            .unwrap();
        let lease_token = uuid::Uuid::new_v4();
        let claimed_at = Utc::now();
        store
            .claim_turn_run(
                lease_token,
                claimed_at,
                claimed_at + chrono::Duration::minutes(1),
            )
            .await
            .unwrap();
        let mut registry = ToolRegistry::new();
        registry.register_client(ToolSpec {
            name: "connect_folder".into(),
            description: "Ask the desktop to connect a folder".into(),
            input_schema: serde_json::json!({"type": "object"}),
        });
        let agent = Agent::new(
            Arc::new(ClientToolProvider {
                assistant_text: false,
                sibling_call: true,
                name: "connect_folder",
                arguments: r#"{"hint":"Documents"}"#,
            }),
            Arc::new(registry),
            store.clone(),
            AgentConfig {
                model: "fake".into(),
                max_steps: 2,
                ..AgentConfig::default()
            },
        )
        .with_durable_steer(lease_token);
        let (tx, _rx) = unbounded();
        let outcome = agent
            .run_claimed_turn(&chat, turn_id, MessageId::new(), 1, &tx)
            .await
            .unwrap();
        assert!(matches!(
            outcome,
            AgentTurnOutcome::Failed {
                error,
                usage: Usage {
                    input_tokens: 10,
                    output_tokens: 4,
                    ..
                },
                model_steps: 2,
            } if error.kind == "max_steps_exceeded"
        ));
        assert!(store.list_tool_calls(chat.id).await.unwrap().is_empty());
    }

    /// The model narrates before it acts. Rejecting a client call for carrying
    /// a preamble spent the whole step budget on a correction the model never
    /// satisfied — the same failure #372 fixed for sensitive calls. The step
    /// must check point instead, keeping the preamble durable across the
    /// resume.
    #[tokio::test]
    async fn client_call_with_prose_checkpoints_and_keeps_the_preamble() {
        let db = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                db.path().join("t.db").display()
            ))
            .await
            .unwrap(),
        );
        let chat = Chat {
            id: ChatId::new(),
            project_id: None,
            title: None,
            model: None,
            reasoning_effort: None,
            attachment_revision: 0,
            root_attachments: Vec::new(),
            created_at: Utc::now(),
        };
        store.create_chat(&chat).await.unwrap();
        let turn_id = TurnId::new();
        store
            .accept_turn(turn_id, chat.id, "fake", "connect documents")
            .await
            .unwrap();
        let lease_token = uuid::Uuid::new_v4();
        let claimed_at = Utc::now();
        store
            .claim_turn_run(
                lease_token,
                claimed_at,
                claimed_at + chrono::Duration::minutes(1),
            )
            .await
            .unwrap();
        let mut registry = ToolRegistry::new();
        registry.register_client(ToolSpec {
            name: "connect_folder".into(),
            description: "Ask the desktop to connect a folder".into(),
            input_schema: serde_json::json!({"type": "object"}),
        });
        let agent = Agent::new(
            Arc::new(ClientToolProvider {
                assistant_text: true,
                sibling_call: false,
                name: "connect_folder",
                arguments: r#"{"hint":"Documents"}"#,
            }),
            Arc::new(registry),
            store.clone(),
            AgentConfig {
                model: "fake".into(),
                max_steps: 2,
                ..AgentConfig::default()
            },
        )
        .with_durable_steer(lease_token);
        let (tx, _rx) = unbounded();
        let outcome = agent
            .run_claimed_turn(&chat, turn_id, MessageId::new(), 1, &tx)
            .await
            .unwrap();

        // One step, not an exhausted budget: the call reached its checkpoint.
        let AgentTurnOutcome::ClientToolCall {
            request,
            model_steps,
            ..
        } = outcome
        else {
            panic!("expected a client tool checkpoint, got {outcome:?}");
        };
        assert_eq!(request.name, "connect_folder");
        assert_eq!(model_steps, 1);

        // The preamble is durable, so the resumed attempt rebuilds it.
        let messages = store.list_messages(chat.id).await.unwrap();
        assert!(
            messages
                .iter()
                .any(|message| message.role == Role::Assistant
                    && message.content.contains("I will connect it")),
            "the assistant preamble should survive the checkpoint: {messages:?}"
        );
    }

    #[tokio::test]
    async fn turn_runs_a_tool_call_then_finishes() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("note.txt"), "hello from disk").unwrap();

        let db = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                db.path().join("t.db").display()
            ))
            .await
            .unwrap(),
        );
        let chat = Chat {
            id: ChatId::new(),
            project_id: None,
            title: None,
            model: None,
            reasoning_effort: None,
            attachment_revision: 0,
            root_attachments: Vec::new(),
            created_at: Utc::now(),
        };
        store.create_chat(&chat).await.unwrap();

        let tools = Arc::new(ToolRegistry::new().with(Box::new(ReadFile)));
        let agent = Agent::new(
            Arc::new(FakeProvider {
                calls: AtomicUsize::new(0),
            }),
            tools,
            store.clone(),
            AgentConfig {
                model: "fake".into(),
                tool_scratch: Some(tool_scratch(workspace.path())),
                ..Default::default()
            },
        );

        let (tx, rx) = unbounded();
        agent.run_turn(&chat, "read note.txt", &tx).await.unwrap();
        drop(tx);
        let events: Vec<AgentEvent> = rx.collect().await;

        // The tool ran against the real workspace file and the turn completed.
        assert!(matches!(
            events.first(),
            Some(AgentEvent::TurnStarted { .. })
        ));
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::ToolCallStarted { name, .. } if name == "read_file"
        )));
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::ToolCallCompleted { output, .. }
                if output.content == "hello from disk" && !output.is_error
        )));
        assert!(events
            .iter()
            .any(|e| matches!(e, AgentEvent::TextDelta { text } if text == "done")));
        // TurnCompleted usage sums both model calls (5+3 in, 2+4 out).
        let usage = events.iter().find_map(|e| match e {
            AgentEvent::TurnCompleted { usage, .. } => Some(*usage),
            _ => None,
        });
        assert_eq!(
            usage.map(|u| (u.input_tokens, u.output_tokens)),
            Some((8, 6))
        );

        // User input and the final answer are text messages; the tool call is
        // a structured row (not Role::Tool).
        let stored = store.list_messages(chat.id).await.unwrap();
        let roles: Vec<Role> = stored.iter().map(|m| m.role).collect();
        assert_eq!(roles, vec![Role::User, Role::Assistant]);
        let calls = store.list_tool_calls(chat.id).await.unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "read_file");
        assert_eq!(calls[0].result.as_deref(), Some("hello from disk"));
        assert_eq!(calls[0].status, ToolCallStatus::Completed);
        assert!(calls[0].resolved_at.is_some());
    }

    #[tokio::test]
    async fn claimed_turn_defers_terminal_publication_to_durable_worker() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("note.txt"), "hello from disk").unwrap();

        let db = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                db.path().join("t.db").display()
            ))
            .await
            .unwrap(),
        );
        let chat = Chat {
            id: ChatId::new(),
            project_id: None,
            title: None,
            model: None,
            reasoning_effort: None,
            attachment_revision: 0,
            root_attachments: Vec::new(),
            created_at: Utc::now(),
        };
        store.create_chat(&chat).await.unwrap();
        let turn_id = TurnId::new();
        store
            .accept_turn(turn_id, chat.id, "fake", "read note.txt")
            .await
            .unwrap();
        let claimed_at = Utc::now();
        let lease_token = uuid::Uuid::new_v4();
        let claimed = store
            .claim_turn_run(
                lease_token,
                claimed_at,
                claimed_at + chrono::Duration::minutes(1),
            )
            .await
            .unwrap()
            .turn
            .expect("accepted turn is claimable");
        assert_eq!(claimed.id, turn_id);

        let agent = Agent::new(
            Arc::new(FakeProvider {
                calls: AtomicUsize::new(0),
            }),
            Arc::new(ToolRegistry::new().with(Box::new(ReadFile))),
            store.clone(),
            AgentConfig {
                model: "fake".into(),
                tool_scratch: Some(tool_scratch(workspace.path())),
                ..Default::default()
            },
        );
        let output_message_id = MessageId::new();
        let (tx, rx) = unbounded();
        let outcome = agent
            .run_claimed_turn(&chat, turn_id, output_message_id, 1, &tx)
            .await
            .unwrap();
        drop(tx);
        let events = emitted_events(rx.collect().await);

        let AgentTurnOutcome::Completed {
            output,
            usage,
            stop_reason,
            ..
        } = outcome
        else {
            panic!("claimed turn should complete");
        };
        assert_eq!(output.id, output_message_id);
        assert_eq!(output.chat_id, chat.id);
        assert_eq!(output.turn_id, turn_id);
        assert_eq!(output.role, Role::Assistant);
        assert_eq!(output.content, "done");
        assert_eq!((usage.input_tokens, usage.output_tokens), (8, 6));
        assert_eq!(stop_reason, StopReason::EndTurn);
        assert!(
            events.iter().all(|event| !matches!(
                event,
                AgentEvent::TurnStarted { .. }
                    | AgentEvent::TurnCompleted { .. }
                    | AgentEvent::TurnCancelled { .. }
            )),
            "the worker owns lifecycle events around the durable execution boundary"
        );

        let stored = store.list_messages(chat.id).await.unwrap();
        assert_eq!(stored.len(), 1, "accepted input must not be duplicated");
        assert_eq!(stored[0].role, Role::User);
        assert_eq!(stored[0].content, "read note.txt");
        assert!(
            stored.iter().all(|message| message.id != output_message_id),
            "final output must remain unpublished until atomic completion"
        );
        let calls = store.list_tool_calls(chat.id).await.unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].turn_id, turn_id);

        for (index, event) in events.iter().enumerate() {
            let ordinal = i32::try_from(index + 1).unwrap();
            assert_eq!(
                store
                    .append_turn_event(chat.id, turn_id, lease_token, ordinal, Utc::now(), event,)
                    .await
                    .unwrap(),
                Some(i64::from(ordinal))
            );
        }

        let completed = store
            .complete_turn_run_and_append_event(
                turn_id,
                lease_token,
                0,
                Utc::now(),
                &output,
                usage,
                stop_reason,
            )
            .await
            .unwrap()
            .expect("the live worker lease can publish its prepared output");
        assert!(matches!(
            completed.outcome,
            crate::CompleteTurnRunOutcome::Completed(_)
        ));
        let terminal = completed
            .terminal_event
            .expect("completion must return its committed terminal event");
        assert_eq!(terminal.seq, i64::try_from(events.len() + 1).unwrap());
        assert_eq!(
            terminal.event,
            AgentEvent::TurnCompleted { usage, stop_reason }
        );
        assert_eq!(
            store.list_events(chat.id, 0).await.unwrap().last(),
            Some(&terminal)
        );
        let recovered = store
            .complete_turn_run_and_append_event(
                turn_id,
                lease_token,
                0,
                claimed_at + chrono::Duration::hours(1),
                &output,
                usage,
                stop_reason,
            )
            .await
            .unwrap()
            .expect("an exact completion retry must remain recoverable");
        assert!(matches!(
            recovered.outcome,
            crate::CompleteTurnRunOutcome::Existing(_)
        ));
        assert_eq!(recovered.terminal_event, Some(terminal));
        let stored = store.list_messages(chat.id).await.unwrap();
        assert_eq!(stored.len(), 2);
        assert_eq!(stored[1].id, output.id);
        assert_eq!(stored[1].chat_id, output.chat_id);
        assert_eq!(stored[1].turn_id, output.turn_id);
        assert_eq!(stored[1].role, output.role);
        assert_eq!(stored[1].content, output.content);
        assert_eq!(
            stored[1].created_at.timestamp_micros(),
            output.created_at.timestamp_micros()
        );

        let failed_turn_id = TurnId::new();
        store
            .accept_turn(
                failed_turn_id,
                chat.id,
                "fake",
                "fail before calling the model",
            )
            .await
            .unwrap();
        let failure_claimed_at = Utc::now();
        let failure_token = uuid::Uuid::new_v4();
        let failed_claim = store
            .claim_turn_run(
                failure_token,
                failure_claimed_at,
                failure_claimed_at + chrono::Duration::minutes(1),
            )
            .await
            .unwrap()
            .turn
            .expect("second accepted turn is claimable");
        assert_eq!(failed_claim.id, failed_turn_id);
        let failing_agent = Agent::new(
            Arc::new(FakeProvider {
                calls: AtomicUsize::new(0),
            }),
            Arc::new(ToolRegistry::new()),
            store.clone(),
            AgentConfig {
                model: "fake".into(),
                max_steps: 0,
                ..Default::default()
            },
        );
        let (failure_tx, failure_rx) = unbounded();
        let error = failing_agent
            .run_claimed_turn(&chat, failed_turn_id, MessageId::new(), 1, &failure_tx)
            .await
            .expect_err("the zero-step guard fails execution");
        drop(failure_tx);
        let failure_events = emitted_events(failure_rx.collect().await);
        assert!(failure_events.iter().all(|event| !matches!(
            event,
            AgentEvent::TurnStarted { .. }
                | AgentEvent::TurnCompleted { .. }
                | AgentEvent::TurnCancelled { .. }
                | AgentEvent::TurnFailed { .. }
        )));
        let error_detail = error.to_string();
        let failure = store
            .record_turn_run_failure_and_append_event(
                failed_turn_id,
                failure_token,
                Utc::now(),
                crate::TurnFailureRetry::Permanent,
                0,
                Usage::default(),
                "agent_error",
                Some(&error_detail),
            )
            .await
            .unwrap()
            .expect("the worker can record failure before publishing its event");
        assert!(matches!(
            failure.outcome,
            crate::RecordTurnFailureOutcome::Recorded(_)
        ));
        let terminal = failure
            .terminal_event
            .expect("terminal failure must return its committed event");
        assert_eq!(
            terminal.event,
            AgentEvent::TurnFailed {
                error: crate::AgentErrorInfo {
                    kind: "agent_error".into(),
                    message: error_detail.clone(),
                }
            }
        );
        assert_eq!(
            store.list_events(chat.id, 0).await.unwrap().last(),
            Some(&terminal)
        );
        let recovered = store
            .record_turn_run_failure_and_append_event(
                failed_turn_id,
                failure_token,
                failure_claimed_at + chrono::Duration::hours(1),
                crate::TurnFailureRetry::Permanent,
                0,
                Usage::default(),
                "agent_error",
                Some(&error_detail),
            )
            .await
            .unwrap()
            .expect("an exact terminal failure retry must remain recoverable");
        assert!(matches!(
            recovered.outcome,
            crate::RecordTurnFailureOutcome::Existing(_)
        ));
        assert_eq!(recovered.terminal_event, Some(terminal));

        let cancelled_turn_id = TurnId::new();
        store
            .accept_turn(
                cancelled_turn_id,
                chat.id,
                "fake",
                "cancel before calling the model",
            )
            .await
            .unwrap();
        let cancellation_claimed_at = Utc::now();
        let cancellation_token = uuid::Uuid::new_v4();
        let cancelled_claim = store
            .claim_turn_run(
                cancellation_token,
                cancellation_claimed_at,
                cancellation_claimed_at + chrono::Duration::minutes(1),
            )
            .await
            .unwrap()
            .turn
            .expect("third accepted turn is claimable");
        assert_eq!(cancelled_claim.id, cancelled_turn_id);
        assert!(matches!(
            store
                .request_turn_cancellation_and_append_event(cancelled_turn_id, Utc::now())
                .await
                .unwrap(),
            Some(crate::JournaledTurnOutcome {
                outcome: crate::RequestTurnCancellationOutcome::Requested(_),
                terminal_event: None,
            })
        ));

        let cancel = CancelToken::new();
        cancel.cancel();
        let cancelled_agent = Agent::new(
            Arc::new(FakeProvider {
                calls: AtomicUsize::new(0),
            }),
            Arc::new(ToolRegistry::new()),
            store.clone(),
            AgentConfig {
                model: "fake".into(),
                ..Default::default()
            },
        )
        .with_cancel(cancel);
        let (cancellation_tx, cancellation_rx) = unbounded();
        let outcome = cancelled_agent
            .run_claimed_turn(
                &chat,
                cancelled_turn_id,
                MessageId::new(),
                1,
                &cancellation_tx,
            )
            .await
            .unwrap();
        drop(cancellation_tx);
        assert_eq!(
            outcome,
            AgentTurnOutcome::Cancelled {
                usage: Usage::default(),
                model_steps: 0,
            }
        );
        let cancellation_events = emitted_events(cancellation_rx.collect().await);
        assert!(cancellation_events.iter().all(|event| !matches!(
            event,
            AgentEvent::TurnStarted { .. }
                | AgentEvent::TurnCompleted { .. }
                | AgentEvent::TurnCancelled { .. }
                | AgentEvent::TurnFailed { .. }
        )));
        let cancellation = store
            .finish_turn_cancellation_and_append_event(
                cancelled_turn_id,
                cancellation_token,
                Utc::now(),
                Usage::default(),
            )
            .await
            .unwrap()
            .expect("the exact worker acknowledgement must commit");
        assert!(matches!(
            cancellation.outcome,
            crate::FinishTurnCancellationOutcome::Cancelled(_)
        ));
        let terminal = cancellation
            .terminal_event
            .expect("terminal cancellation must return its committed event");
        assert_eq!(
            terminal.event,
            AgentEvent::TurnCancelled {
                usage: Usage::default()
            }
        );
        assert_eq!(
            store.list_events(chat.id, 0).await.unwrap().last(),
            Some(&terminal)
        );
        let recovered = store
            .finish_turn_cancellation_and_append_event(
                cancelled_turn_id,
                cancellation_token,
                cancellation_claimed_at + chrono::Duration::hours(1),
                Usage::default(),
            )
            .await
            .unwrap()
            .expect("an exact cancellation retry must remain recoverable");
        assert!(matches!(
            recovered.outcome,
            crate::FinishTurnCancellationOutcome::Existing(_)
        ));
        assert_eq!(recovered.terminal_event, Some(terminal));
    }

    #[tokio::test]
    async fn tool_context_inherits_the_chats_project_scope() {
        let db = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                db.path().join("t.db").display()
            ))
            .await
            .unwrap(),
        );
        let project = Project {
            id: ProjectId::new(),
            title: None,
            attachment_revision: 0,
            root_attachments: Vec::new(),
            created_at: Utc::now(),
        };
        store.create_project(&project).await.unwrap();
        let chat = Chat {
            id: ChatId::new(),
            project_id: Some(project.id),
            title: None,
            model: None,
            reasoning_effort: None,
            attachment_revision: 0,
            root_attachments: Vec::new(),
            created_at: Utc::now(),
        };
        store.create_chat(&chat).await.unwrap();
        let observed_project = Arc::new(Mutex::new(None));
        let observed_call = Arc::new(Mutex::new(None));
        let tools = Arc::new(ToolRegistry::new().with(Box::new(ContextRecordingTool {
            observed_project: observed_project.clone(),
            observed_call: observed_call.clone(),
        })));
        let agent = Agent::new(
            Arc::new(FakeProvider {
                calls: AtomicUsize::new(0),
            }),
            tools,
            store,
            AgentConfig {
                model: "fake".into(),
                ..Default::default()
            },
        );

        let (tx, _rx) = unbounded();
        agent.run_turn(&chat, "inspect context", &tx).await.unwrap();
        assert_eq!(*observed_project.lock().unwrap(), Some(Some(project.id)));
        assert!(
            observed_call.lock().unwrap().is_some(),
            "provider adapters need the canonical call id for reconciliation"
        );
    }

    #[tokio::test]
    async fn max_steps_guard_fails_before_running_tools() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("note.txt"), "secret").unwrap();
        let db = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                db.path().join("t.db").display()
            ))
            .await
            .unwrap(),
        );
        let chat = Chat {
            id: ChatId::new(),
            project_id: None,
            title: None,
            model: None,
            reasoning_effort: None,
            attachment_revision: 0,
            root_attachments: Vec::new(),
            created_at: Utc::now(),
        };
        store.create_chat(&chat).await.unwrap();

        // Only one step allowed, but step 0 returns a tool call — there's no
        // step left to consume the result, so the tool must NOT run.
        let agent = Agent::new(
            Arc::new(FakeProvider {
                calls: AtomicUsize::new(0),
            }),
            Arc::new(ToolRegistry::new().with(Box::new(ReadFile))),
            store.clone(),
            AgentConfig {
                model: "fake".into(),
                max_steps: 1,
                ..Default::default()
            },
        );

        let (tx, rx) = unbounded();
        let result = agent.run_turn(&chat, "read note.txt", &tx).await;
        drop(tx);
        let events: Vec<AgentEvent> = rx.collect().await;

        assert!(result.is_err());
        assert!(matches!(
            events.first(),
            Some(AgentEvent::TurnStarted { .. })
        ));
        assert!(matches!(events.last(), Some(AgentEvent::TurnFailed { .. })));
        // The tool never ran: no completion event and nothing tool-related persisted.
        assert!(!events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolCallCompleted { .. })));
        let roles: Vec<Role> = store
            .list_messages(chat.id)
            .await
            .unwrap()
            .iter()
            .map(|m| m.role)
            .collect();
        assert_eq!(roles, vec![Role::User]);
    }

    #[tokio::test]
    async fn claimed_max_steps_failure_retains_provider_progress() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("note.txt"), "secret").unwrap();
        let db = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                db.path().join("t.db").display()
            ))
            .await
            .unwrap(),
        );
        let chat = Chat {
            id: ChatId::new(),
            project_id: None,
            title: None,
            model: None,
            reasoning_effort: None,
            attachment_revision: 0,
            root_attachments: Vec::new(),
            created_at: Utc::now(),
        };
        store.create_chat(&chat).await.unwrap();
        let turn_id = TurnId::new();
        store
            .accept_turn(turn_id, chat.id, "fake", "read note.txt")
            .await
            .unwrap();
        let claimed_at = Utc::now();
        let lease_token = uuid::Uuid::new_v4();
        store
            .claim_turn_run(
                lease_token,
                claimed_at,
                claimed_at + chrono::Duration::minutes(1),
            )
            .await
            .unwrap();
        let agent = Agent::new(
            Arc::new(FakeProvider {
                calls: AtomicUsize::new(0),
            }),
            Arc::new(ToolRegistry::new().with(Box::new(ReadFile))),
            store.clone(),
            AgentConfig {
                model: "fake".into(),
                max_steps: 1,
                ..Default::default()
            },
        )
        .with_durable_steer(lease_token);
        let (tx, _rx) = unbounded();
        let outcome = agent
            .run_claimed_turn(&chat, turn_id, MessageId::new(), 1, &tx)
            .await
            .unwrap();
        assert!(matches!(
            outcome,
            AgentTurnOutcome::Failed {
                error,
                usage: Usage {
                    input_tokens: 5,
                    output_tokens: 2,
                    ..
                },
                model_steps: 1,
            } if error.kind == "message" && error.message == "max steps per turn exceeded"
        ));
        let calls = store.list_tool_calls(chat.id).await.unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].status, ToolCallStatus::Pending);
        assert_eq!(calls[0].result, None);
    }

    #[tokio::test]
    async fn large_tool_results_are_truncated() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("note.txt"), "x".repeat(10_000)).unwrap();
        let db = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                db.path().join("t.db").display()
            ))
            .await
            .unwrap(),
        );
        let chat = Chat {
            id: ChatId::new(),
            project_id: None,
            title: None,
            model: None,
            reasoning_effort: None,
            attachment_revision: 0,
            root_attachments: Vec::new(),
            created_at: Utc::now(),
        };
        store.create_chat(&chat).await.unwrap();

        let agent = Agent::new(
            Arc::new(FakeProvider {
                calls: AtomicUsize::new(0),
            }),
            Arc::new(ToolRegistry::new().with(Box::new(ReadFile))),
            store,
            AgentConfig {
                model: "fake".into(),
                max_tool_result_bytes: 100,
                tool_scratch: Some(tool_scratch(workspace.path())),
                ..Default::default()
            },
        );

        let (tx, rx) = unbounded();
        agent.run_turn(&chat, "read note.txt", &tx).await.unwrap();
        drop(tx);
        let events: Vec<AgentEvent> = rx.collect().await;

        let output = events
            .iter()
            .find_map(|e| match e {
                AgentEvent::ToolCallCompleted { output, .. } => Some(output.clone()),
                _ => None,
            })
            .expect("a tool completed");
        assert!(!output.is_error);
        assert!(output.content.len() < 10_000, "result should be capped");
        assert!(output.content.contains("[truncated:"));
    }

    /// A Sensitive tool that records whether it ran.
    struct BoomTool {
        ran: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Tool for BoomTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "boom".into(),
                description: "a sensitive tool".into(),
                input_schema: serde_json::json!({"type": "object"}),
            }
        }
        fn approval_class(&self) -> ApprovalClass {
            ApprovalClass::Sensitive
        }
        async fn execute(&self, _ctx: &ToolCtx, _args: Value) -> Result<ToolOutput> {
            self.ran.fetch_add(1, Ordering::SeqCst);
            Ok(ToolOutput::text("boomed"))
        }
    }

    /// Provider that always asks for the `boom` tool once, then finishes.
    struct BoomProvider {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl ModelProvider for BoomProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("boom")
        }
        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            let events = if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                vec![
                    ProviderEvent::ToolCallStarted {
                        index: 0,
                        id: "call_boom".into(),
                        name: "boom".into(),
                    },
                    ProviderEvent::ToolCallArgsDelta {
                        index: 0,
                        fragment: "{}".into(),
                    },
                    ProviderEvent::Stop {
                        reason: StopReason::ToolUse,
                    },
                ]
            } else {
                vec![
                    ProviderEvent::TextDelta { text: "ok".into() },
                    ProviderEvent::Stop {
                        reason: StopReason::EndTurn,
                    },
                ]
            };
            Ok(stream::iter(events).boxed())
        }
    }

    #[tokio::test]
    async fn sensitive_tool_parks_until_approved() {
        use crate::approval::AutoApproveGate;

        let db = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                db.path().join("t.db").display()
            ))
            .await
            .unwrap(),
        );
        let chat = Chat {
            id: ChatId::new(),
            project_id: None,
            title: None,
            model: None,
            reasoning_effort: None,
            attachment_revision: 0,
            root_attachments: Vec::new(),
            created_at: Utc::now(),
        };
        store.create_chat(&chat).await.unwrap();

        let ran = Arc::new(AtomicUsize::new(0));
        let tools = Arc::new(ToolRegistry::new().with(Box::new(BoomTool { ran: ran.clone() })));
        let agent = Agent::new(
            Arc::new(BoomProvider {
                calls: AtomicUsize::new(0),
            }),
            tools,
            store,
            AgentConfig {
                model: "fake".into(),
                ..Default::default()
            },
        )
        .with_approvals(Arc::new(AutoApproveGate));

        let (tx, rx) = unbounded();
        agent.run_turn(&chat, "go", &tx).await.unwrap();
        drop(tx);
        let events: Vec<AgentEvent> = rx.collect().await;

        assert!(events
            .iter()
            .any(|e| matches!(e, AgentEvent::ApprovalRequired { .. })));
        assert!(events
            .iter()
            .any(|e| matches!(e, AgentEvent::ApprovalDecided { approved: true, .. })));
        assert_eq!(ran.load(Ordering::SeqCst), 1);
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::ToolCallCompleted { output, .. }
                if output.content == "boomed" && !output.is_error
        )));
    }

    /// Provider that prefaces a sensitive `boom` call with prose, then
    /// finishes on the next step.
    struct ProseBoomProvider {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl ModelProvider for ProseBoomProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("prose-boom")
        }
        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            let events = if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                vec![
                    ProviderEvent::TextDelta {
                        text: "I'll run the sensitive tool for you.".into(),
                    },
                    ProviderEvent::ToolCallStarted {
                        index: 0,
                        id: "call_boom".into(),
                        name: "boom".into(),
                    },
                    ProviderEvent::ToolCallArgsDelta {
                        index: 0,
                        fragment: "{}".into(),
                    },
                    ProviderEvent::Stop {
                        reason: StopReason::ToolUse,
                    },
                ]
            } else {
                vec![
                    ProviderEvent::TextDelta { text: "ok".into() },
                    ProviderEvent::Stop {
                        reason: StopReason::EndTurn,
                    },
                ]
            };
            Ok(stream::iter(events).boxed())
        }
    }

    /// The failure that motivated #372: prose plus one sensitive call must
    /// keep the preamble, persist it like any other text+tool step, and reach
    /// the approval gate on the first step instead of burning the budget on
    /// corrective retries.
    #[tokio::test]
    async fn sensitive_call_with_prose_keeps_the_preamble_and_parks() {
        use crate::approval::AutoApproveGate;

        let db = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                db.path().join("t.db").display()
            ))
            .await
            .unwrap(),
        );
        let chat = Chat {
            id: ChatId::new(),
            project_id: None,
            title: None,
            model: None,
            reasoning_effort: None,
            attachment_revision: 0,
            root_attachments: Vec::new(),
            created_at: Utc::now(),
        };
        store.create_chat(&chat).await.unwrap();

        let ran = Arc::new(AtomicUsize::new(0));
        let tools = Arc::new(ToolRegistry::new().with(Box::new(BoomTool { ran: ran.clone() })));
        let provider = Arc::new(ProseBoomProvider {
            calls: AtomicUsize::new(0),
        });
        let agent = Agent::new(
            provider.clone(),
            tools,
            store.clone(),
            AgentConfig {
                model: "fake".into(),
                ..Default::default()
            },
        )
        .with_approvals(Arc::new(AutoApproveGate));

        let (tx, rx) = unbounded();
        agent.run_turn(&chat, "go", &tx).await.unwrap();
        drop(tx);
        let events: Vec<AgentEvent> = rx.collect().await;

        // The step is never rejected or scrubbed: the streamed preamble stands.
        assert!(!events
            .iter()
            .any(|e| matches!(e, AgentEvent::StreamInterrupted)));
        // The call parks on the first step and runs once approved.
        assert!(events
            .iter()
            .any(|e| matches!(e, AgentEvent::ApprovalRequired { .. })));
        assert_eq!(ran.load(Ordering::SeqCst), 1);
        // No corrective retry: the tool step plus the closing step.
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
        // The preamble is persisted exactly once, like any other text+tool step.
        let history = store.list_messages(chat.id).await.unwrap();
        assert_eq!(
            history
                .iter()
                .filter(|message| message.content.contains("sensitive tool for you"))
                .count(),
            1
        );
    }

    /// Provider that pairs the sensitive call with a sibling call, which is
    /// still malformed and must keep the corrective retry.
    struct SiblingBoomProvider {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl ModelProvider for SiblingBoomProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("sibling-boom")
        }
        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            let events = if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                vec![
                    ProviderEvent::ToolCallStarted {
                        index: 0,
                        id: "call_a".into(),
                        name: "boom".into(),
                    },
                    ProviderEvent::ToolCallArgsDelta {
                        index: 0,
                        fragment: "{}".into(),
                    },
                    ProviderEvent::ToolCallStarted {
                        index: 1,
                        id: "call_b".into(),
                        name: "boom".into(),
                    },
                    ProviderEvent::ToolCallArgsDelta {
                        index: 1,
                        fragment: "{}".into(),
                    },
                    ProviderEvent::Stop {
                        reason: StopReason::ToolUse,
                    },
                ]
            } else {
                vec![
                    ProviderEvent::TextDelta { text: "ok".into() },
                    ProviderEvent::Stop {
                        reason: StopReason::EndTurn,
                    },
                ]
            };
            Ok(stream::iter(events).boxed())
        }
    }

    #[tokio::test]
    async fn sensitive_call_with_sibling_calls_still_retries() {
        use crate::approval::AutoApproveGate;

        let db = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                db.path().join("t.db").display()
            ))
            .await
            .unwrap(),
        );
        let chat = Chat {
            id: ChatId::new(),
            project_id: None,
            title: None,
            model: None,
            reasoning_effort: None,
            attachment_revision: 0,
            root_attachments: Vec::new(),
            created_at: Utc::now(),
        };
        store.create_chat(&chat).await.unwrap();

        let ran = Arc::new(AtomicUsize::new(0));
        let tools = Arc::new(ToolRegistry::new().with(Box::new(BoomTool { ran: ran.clone() })));
        let provider = Arc::new(SiblingBoomProvider {
            calls: AtomicUsize::new(0),
        });
        let agent = Agent::new(
            provider.clone(),
            tools,
            store,
            AgentConfig {
                model: "fake".into(),
                ..Default::default()
            },
        )
        .with_approvals(Arc::new(AutoApproveGate));

        let (tx, rx) = unbounded();
        agent.run_turn(&chat, "go", &tx).await.unwrap();
        drop(tx);
        let events: Vec<AgentEvent> = rx.collect().await;

        assert!(events
            .iter()
            .any(|e| matches!(e, AgentEvent::StreamInterrupted)));
        assert!(!events
            .iter()
            .any(|e| matches!(e, AgentEvent::ApprovalRequired { .. })));
        assert_eq!(ran.load(Ordering::SeqCst), 0);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    }

    /// A Sensitive, standing-grantable tool (`search`) that records whether it
    /// ran.
    struct SearchTool {
        ran: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Tool for SearchTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "search".into(),
                description: "a sensitive search tool".into(),
                input_schema: serde_json::json!({"type": "object"}),
            }
        }
        fn approval_class(&self) -> ApprovalClass {
            ApprovalClass::Sensitive
        }
        async fn execute(&self, _ctx: &ToolCtx, _args: Value) -> Result<ToolOutput> {
            self.ran.fetch_add(1, Ordering::SeqCst);
            Ok(ToolOutput::text("searched"))
        }
    }

    /// Provider that asks for the `search` tool once, then finishes.
    struct SearchProvider {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl ModelProvider for SearchProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("search")
        }
        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            let events = if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                vec![
                    ProviderEvent::ToolCallStarted {
                        index: 0,
                        id: "call_search".into(),
                        name: "search".into(),
                    },
                    ProviderEvent::ToolCallArgsDelta {
                        index: 0,
                        fragment: "{}".into(),
                    },
                    ProviderEvent::Stop {
                        reason: StopReason::ToolUse,
                    },
                ]
            } else {
                vec![
                    ProviderEvent::TextDelta { text: "ok".into() },
                    ProviderEvent::Stop {
                        reason: StopReason::EndTurn,
                    },
                ]
            };
            Ok(stream::iter(events).boxed())
        }
    }

    async fn search_grant_chat(store: &Arc<dyn Store>) -> Chat {
        let chat = Chat {
            id: ChatId::new(),
            project_id: None,
            title: None,
            model: None,
            reasoning_effort: None,
            attachment_revision: 0,
            root_attachments: Vec::new(),
            created_at: Utc::now(),
        };
        store.create_chat(&chat).await.unwrap();
        chat
    }

    async fn search_grant_store() -> Arc<dyn Store> {
        let db = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                db.path().join("t.db").display()
            ))
            .await
            .unwrap(),
        );
        // Keep the temp dir alive for the process; SQLite owns its connection.
        std::mem::forget(db);
        store
    }

    fn search_agent(
        store: Arc<dyn Store>,
        ran: Arc<AtomicUsize>,
        grants: Arc<crate::approval::StandingGrants>,
    ) -> Agent {
        let tools = Arc::new(ToolRegistry::new().with(Box::new(SearchTool { ran })));
        // Default gate is `RefuseGate`: it rejects any call that reaches it, so
        // the tool running proves the standing grant bypassed the gate entirely.
        Agent::new(
            Arc::new(SearchProvider {
                calls: AtomicUsize::new(0),
            }),
            tools,
            store,
            AgentConfig {
                model: "fake".into(),
                ..Default::default()
            },
        )
        .with_standing_grants(grants)
    }

    #[tokio::test]
    async fn standing_grant_runs_sensitive_tool_without_parking() {
        use crate::approval::{StandingGrant, StandingGrants};

        let store = search_grant_store().await;
        let chat = search_grant_chat(&store).await;
        let grants = Arc::new(StandingGrants::from_grants(vec![StandingGrant::new(
            chat.id,
            "search",
            ToolApprovalKind::for_tool_name("search"),
            Utc::now(),
        )
        .expect("search is grantable")]));

        let ran = Arc::new(AtomicUsize::new(0));
        let agent = search_agent(store, ran.clone(), grants);

        let (tx, rx) = unbounded();
        agent.run_turn(&chat, "go", &tx).await.unwrap();
        drop(tx);
        let events: Vec<AgentEvent> = rx.collect().await;

        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AgentEvent::ApprovalRequired { .. })),
            "a covered call must not re-prompt"
        );
        assert_eq!(ran.load(Ordering::SeqCst), 1);
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::ToolCallCompleted { output, .. }
                if output.content == "searched" && !output.is_error
        )));
    }

    #[tokio::test]
    async fn standing_grant_for_another_chat_does_not_bypass_the_gate() {
        use crate::approval::{StandingGrant, StandingGrants};

        let store = search_grant_store().await;
        let chat = search_grant_chat(&store).await;
        // A grant scoped to a different chat must not cover this chat's call.
        let grants = Arc::new(StandingGrants::from_grants(vec![StandingGrant::new(
            ChatId::new(),
            "search",
            ToolApprovalKind::for_tool_name("search"),
            Utc::now(),
        )
        .expect("search is grantable")]));

        let ran = Arc::new(AtomicUsize::new(0));
        let agent = search_agent(store, ran.clone(), grants);

        let (tx, rx) = unbounded();
        agent.run_turn(&chat, "go", &tx).await.unwrap();
        drop(tx);
        let events: Vec<AgentEvent> = rx.collect().await;

        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::ApprovalRequired { .. })),
            "an uncovered call must still park on the gate"
        );
        assert_eq!(ran.load(Ordering::SeqCst), 0, "RefuseGate blocks the tool");
    }

    /// A Sensitive tool that escapes the chat workspace (`exec`) and records
    /// whether it ran.
    struct ExecTool {
        ran: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Tool for ExecTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "exec".into(),
                description: "an escaping command execution tool".into(),
                input_schema: serde_json::json!({"type": "object"}),
            }
        }
        fn approval_class(&self) -> ApprovalClass {
            ApprovalClass::Sensitive
        }
        async fn execute(&self, _ctx: &ToolCtx, _args: Value) -> Result<ToolOutput> {
            self.ran.fetch_add(1, Ordering::SeqCst);
            Ok(ToolOutput::text("executed"))
        }
    }

    /// Provider that asks for the `exec` tool once, then finishes.
    struct ExecProvider {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl ModelProvider for ExecProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("exec")
        }
        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            let events = if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                vec![
                    ProviderEvent::ToolCallStarted {
                        index: 0,
                        id: "call_exec".into(),
                        name: "exec".into(),
                    },
                    ProviderEvent::ToolCallArgsDelta {
                        index: 0,
                        fragment: "{}".into(),
                    },
                    ProviderEvent::Stop {
                        reason: StopReason::ToolUse,
                    },
                ]
            } else {
                vec![
                    ProviderEvent::TextDelta { text: "ok".into() },
                    ProviderEvent::Stop {
                        reason: StopReason::EndTurn,
                    },
                ]
            };
            Ok(stream::iter(events).boxed())
        }
    }

    fn exec_agent(
        store: Arc<dyn Store>,
        ran: Arc<AtomicUsize>,
        grants: Arc<StandingGrants>,
    ) -> Agent {
        let tools = Arc::new(ToolRegistry::new().with(Box::new(ExecTool { ran })));
        // Default gate is `RefuseGate`: it rejects any call that reaches it, so
        // the tool running proves the standing grant bypassed the gate entirely.
        Agent::new(
            Arc::new(ExecProvider {
                calls: AtomicUsize::new(0),
            }),
            tools,
            store,
            AgentConfig {
                model: "fake".into(),
                ..Default::default()
            },
        )
        .with_standing_grants(grants)
    }

    #[tokio::test]
    async fn standing_grant_runs_escaping_exec_without_parking() {
        use crate::approval::{StandingGrant, StandingGrants};

        let store = search_grant_store().await;
        let chat = search_grant_chat(&store).await;
        let grants = Arc::new(StandingGrants::from_grants(vec![StandingGrant::new(
            chat.id,
            "exec",
            ToolApprovalKind::for_tool_name("exec"),
            Utc::now(),
        )
        .expect("exec is grantable")]));

        let ran = Arc::new(AtomicUsize::new(0));
        let agent = exec_agent(store, ran.clone(), grants);

        let (tx, rx) = unbounded();
        agent.run_turn(&chat, "go", &tx).await.unwrap();
        drop(tx);
        let events: Vec<AgentEvent> = rx.collect().await;

        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AgentEvent::ApprovalRequired { .. })),
            "a covered escaping call must not re-prompt"
        );
        assert_eq!(ran.load(Ordering::SeqCst), 1);
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::ToolCallCompleted { output, .. }
                if output.content == "executed" && !output.is_error
        )));
    }

    #[tokio::test]
    async fn ungranted_escaping_exec_still_parks_deny_by_default() {
        use crate::approval::StandingGrants;

        let store = search_grant_store().await;
        let chat = search_grant_chat(&store).await;
        // No grant covers this chat: an escaping action must still park.
        let grants = Arc::new(StandingGrants::new());

        let ran = Arc::new(AtomicUsize::new(0));
        let agent = exec_agent(store, ran.clone(), grants);

        let (tx, rx) = unbounded();
        agent.run_turn(&chat, "go", &tx).await.unwrap();
        drop(tx);
        let events: Vec<AgentEvent> = rx.collect().await;

        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::ApprovalRequired { kind, .. }
                    if *kind == ToolApprovalKind::ExecMayRunNetworkedCommand
            )),
            "an uncovered escaping call must park on the gate with a presentable kind"
        );
        assert_eq!(ran.load(Ordering::SeqCst), 0, "RefuseGate blocks the tool");
    }

    /// Counts every execution so a test can prove a fenced tool never ran.
    struct SpyTool {
        ran: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Tool for SpyTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "spy".into(),
                description: "records whether it executed".into(),
                input_schema: serde_json::json!({"type": "object"}),
            }
        }
        fn approval_class(&self) -> ApprovalClass {
            ApprovalClass::ReadOnly
        }
        async fn execute(&self, _ctx: &ToolCtx, _args: Value) -> Result<ToolOutput> {
            self.ran.fetch_add(1, Ordering::SeqCst);
            Ok(ToolOutput::text("spied"))
        }
    }

    /// Asks for the `spy` tool once, but first lets the turn's lease be stolen
    /// while this provider call is in flight: a fresh claim scan past the lease
    /// expiry starts the retry attempt under a new token.
    struct LeaseStealingProvider {
        store: Arc<dyn Store>,
        steal_at: DateTime<Utc>,
        stole: AtomicUsize,
    }

    #[async_trait]
    impl ModelProvider for LeaseStealingProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("lease-steal")
        }
        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            if self.stole.fetch_add(1, Ordering::SeqCst) == 0 {
                let outcome = self
                    .store
                    .claim_turn_run(
                        uuid::Uuid::new_v4(),
                        self.steal_at,
                        self.steal_at + chrono::Duration::minutes(1),
                    )
                    .await?;
                assert!(
                    outcome.turn.is_some(),
                    "expired turn should be reclaimed for a retry by the steal"
                );
            }
            Ok(stream::iter(vec![
                ProviderEvent::ToolCallStarted {
                    index: 0,
                    id: "call_spy".into(),
                    name: "spy".into(),
                },
                ProviderEvent::ToolCallArgsDelta {
                    index: 0,
                    fragment: "{}".into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::ToolUse,
                },
            ])
            .boxed())
        }
    }

    struct AnswerOnlyProvider;

    #[async_trait]
    impl ModelProvider for AnswerOnlyProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("answer-only")
        }

        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            Ok(stream::iter(vec![
                ProviderEvent::TextDelta {
                    text: "recovered".into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ])
            .boxed())
        }
    }

    #[tokio::test]
    async fn a_stolen_lease_fences_intermediate_tool_effects() {
        let db = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                db.path().join("t.db").display()
            ))
            .await
            .unwrap(),
        );
        let chat = Chat {
            id: ChatId::new(),
            project_id: None,
            title: None,
            model: None,
            reasoning_effort: None,
            attachment_revision: 0,
            root_attachments: Vec::new(),
            created_at: Utc::now(),
        };
        store.create_chat(&chat).await.unwrap();
        let turn_id = TurnId::new();
        store
            .accept_turn(turn_id, chat.id, "fake", "go")
            .await
            .unwrap();
        let now = Utc::now();
        let lease_token = uuid::Uuid::new_v4();
        store
            .claim_turn_run(lease_token, now, now + chrono::Duration::minutes(1))
            .await
            .unwrap();

        let ran = Arc::new(AtomicUsize::new(0));
        let tools = Arc::new(ToolRegistry::new().with(Box::new(SpyTool { ran: ran.clone() })));
        let agent = Agent::new(
            Arc::new(LeaseStealingProvider {
                store: store.clone(),
                // The steal reads a claim time past the lease expiry, so the
                // scan reclaims and terminalizes the turn deterministically.
                steal_at: now + chrono::Duration::minutes(2),
                stole: AtomicUsize::new(0),
            }),
            tools,
            store.clone(),
            AgentConfig {
                model: "fake".into(),
                ..Default::default()
            },
        )
        .with_durable_steer(lease_token);

        let (tx, rx) = unbounded();
        let outcome = agent
            .run_claimed_turn(&chat, turn_id, MessageId::new(), 1, &tx)
            .await
            .unwrap();
        drop(tx);
        let _ = rx.collect::<Vec<_>>().await;

        // The stale segment refuses to persist tool-call rows or run the tool.
        assert!(
            matches!(outcome, AgentTurnOutcome::Failed { .. }),
            "a stolen lease must not complete the turn: {outcome:?}"
        );
        assert_eq!(
            ran.load(Ordering::SeqCst),
            0,
            "a stolen lease must not execute tool side effects"
        );
        // The retry claim stands; the stale worker committed nothing.
        let turn = store.get_turn_run(turn_id).await.unwrap().unwrap();
        assert_eq!(turn.status, TurnRunStatus::Running);
        assert_ne!(turn.lease_token, Some(lease_token));
    }

    #[tokio::test]
    async fn retry_abandons_an_inherited_pending_tool_without_replaying_it() {
        let db = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                db.path().join("t.db").display()
            ))
            .await
            .unwrap(),
        );
        let chat = Chat {
            id: ChatId::new(),
            project_id: None,
            title: None,
            model: None,
            reasoning_effort: None,
            attachment_revision: 0,
            root_attachments: Vec::new(),
            created_at: Utc::now(),
        };
        store.create_chat(&chat).await.unwrap();
        let turn_id = TurnId::new();
        let accepted = match store
            .accept_turn(turn_id, chat.id, "fake", "go")
            .await
            .unwrap()
        {
            crate::storage::AcceptTurnOutcome::Accepted(turn) => turn,
            outcome => panic!("unexpected acceptance: {outcome:?}"),
        };
        let first_claim_at = accepted.available_at;
        let first_lease = uuid::Uuid::new_v4();
        store
            .claim_turn_run(
                first_lease,
                first_claim_at,
                first_claim_at + chrono::Duration::seconds(1),
            )
            .await
            .unwrap();
        let call_id = CallId::new();
        let call = ToolCallRecord {
            id: call_id,
            chat_id: chat.id,
            turn_id,
            provider_id: "call_spy".into(),
            name: "spy".into(),
            arguments: serde_json::json!({}),
            execution: ToolCallExecution::Server,
            status: ToolCallStatus::Pending,
            result: None,
            error_code: None,
            error_detail: None,
            client_executor_id: None,
            client_lease_expires_at: None,
            created_at: first_claim_at,
            resolved_at: None,
        };
        assert!(matches!(
            store
                .accept_claimed_tool_call(&call, first_lease, first_claim_at)
                .await
                .unwrap(),
            AcceptClaimedToolCallOutcome::Accepted(_)
        ));

        // Simulate a crash after acceptance and possible execution but before
        // result commit. Reclaiming creates the next failure attempt.
        let retry_at = first_claim_at + chrono::Duration::seconds(2);
        let retry_lease = uuid::Uuid::new_v4();
        let retried = store
            .claim_turn_run(
                retry_lease,
                retry_at,
                retry_at + chrono::Duration::minutes(1),
            )
            .await
            .unwrap()
            .turn
            .unwrap();
        assert_eq!(retried.attempt_count, 2);

        let ran = Arc::new(AtomicUsize::new(0));
        let tools = Arc::new(ToolRegistry::new().with(Box::new(SpyTool { ran: ran.clone() })));
        let agent = Agent::new(
            Arc::new(AnswerOnlyProvider),
            tools,
            store.clone(),
            AgentConfig {
                model: "fake".into(),
                ..Default::default()
            },
        )
        .with_durable_steer(retry_lease);
        let (tx, rx) = unbounded();
        let outcome = agent
            .run_claimed_turn(&chat, turn_id, MessageId::new(), 1, &tx)
            .await
            .unwrap();
        drop(tx);
        let _ = rx.collect::<Vec<_>>().await;

        assert!(matches!(outcome, AgentTurnOutcome::Completed { .. }));
        assert_eq!(ran.load(Ordering::SeqCst), 0, "pending work was replayed");
        let stored = store
            .list_tool_calls(chat.id)
            .await
            .unwrap()
            .into_iter()
            .find(|item| item.id == call_id)
            .unwrap();
        assert_eq!(stored.status, ToolCallStatus::Failed);
        assert_eq!(
            stored.error_code.as_deref(),
            Some("tool_execution_interrupted")
        );
    }

    /// Streams one text delta, then stalls forever — lets a test cancel mid-stream
    /// at a known point (after the delta lands).
    struct StallProvider;

    #[async_trait]
    impl ModelProvider for StallProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("stall")
        }
        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            let head = stream::iter(vec![ProviderEvent::TextDelta {
                text: "partial".into(),
            }]);
            Ok(head.chain(stream::pending()).boxed())
        }
    }

    /// Gate that signals once a call is parked, then never resolves — so a test
    /// can cancel a turn while it is genuinely waiting on approval.
    struct SignalPendingGate {
        armed: std::sync::Mutex<Option<futures::channel::oneshot::Sender<()>>>,
    }

    impl ApprovalGate for SignalPendingGate {
        fn register(
            &self,
            _request: ApprovalRequest,
            _journal: Option<crate::approval::ApprovalJournalIdentity>,
        ) -> crate::approval::ApprovalRegistrationFuture<'_> {
            Box::pin(async move {
                if let Some(tx) = self.armed.lock().unwrap().take() {
                    let _ = tx.send(());
                }
                crate::approval::ApprovalRegistration {
                    decision: Box::pin(future::pending()) as crate::approval::ApprovalFuture,
                    publication: crate::approval::ApprovalRequiredPublication::Ordinary,
                }
            })
        }
    }

    /// Trips cancel, then resolves Approve immediately — both arms of the
    /// approval `select` are ready in the same poll. Without a cancel-preferring
    /// check, `select` would take Approve and the Sensitive tool would run.
    struct CancelThenApproveGate {
        cancel: CancelToken,
    }

    impl ApprovalGate for CancelThenApproveGate {
        fn register(
            &self,
            _request: ApprovalRequest,
            _journal: Option<crate::approval::ApprovalJournalIdentity>,
        ) -> crate::approval::ApprovalRegistrationFuture<'_> {
            Box::pin(async move {
                self.cancel.cancel();
                crate::approval::ApprovalRegistration {
                    decision: Box::pin(async { ApprovalDecision::Approve })
                        as crate::approval::ApprovalFuture,
                    publication: crate::approval::ApprovalRequiredPublication::Ordinary,
                }
            })
        }
    }

    async fn cancel_test_chat() -> (Arc<dyn Store>, Chat, tempfile::TempDir) {
        let workspace = tempfile::tempdir().unwrap();
        let db = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                db.path().join("t.db").display()
            ))
            .await
            .unwrap(),
        );
        let chat = Chat {
            id: ChatId::new(),
            project_id: None,
            title: None,
            model: None,
            reasoning_effort: None,
            attachment_revision: 0,
            root_attachments: Vec::new(),
            created_at: Utc::now(),
        };
        store.create_chat(&chat).await.unwrap();
        (store, chat, workspace)
    }

    struct ToolFutureDropMarker(Arc<AtomicBool>);

    impl Drop for ToolFutureDropMarker {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    struct BlockingTool {
        entered: Arc<tokio::sync::Notify>,
        dropped: Arc<AtomicBool>,
    }

    #[async_trait]
    impl Tool for BlockingTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "blocking".into(),
                description: "wait until the turn is cancelled".into(),
                input_schema: serde_json::json!({"type": "object"}),
            }
        }

        fn approval_class(&self) -> ApprovalClass {
            ApprovalClass::ReadOnly
        }

        async fn execute(&self, _ctx: &ToolCtx, _args: Value) -> Result<ToolOutput> {
            let _drop = ToolFutureDropMarker(self.dropped.clone());
            self.entered.notify_one();
            future::pending().await
        }
    }

    struct BlockingToolProvider;

    #[async_trait]
    impl ModelProvider for BlockingToolProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("blocking-tool")
        }

        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            Ok(stream::iter(vec![
                ProviderEvent::ToolCallStarted {
                    index: 0,
                    id: "blocking_1".into(),
                    name: "blocking".into(),
                },
                ProviderEvent::ToolCallArgsDelta {
                    index: 0,
                    fragment: "{}".into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::ToolUse,
                },
            ])
            .boxed())
        }
    }

    #[tokio::test]
    async fn cancel_before_the_turn_stops_before_any_model_call() {
        let (store, chat, _workspace) = cancel_test_chat().await;

        // A provider whose stream would panic the test if ever polled — proving
        // the loop-top check short-circuits before the first model call.
        let provider = FakeProvider {
            calls: AtomicUsize::new(0),
        };
        let cancel = CancelToken::new();
        cancel.cancel();
        let agent = Agent::new(
            Arc::new(provider),
            Arc::new(ToolRegistry::new()),
            store,
            AgentConfig {
                model: "fake".into(),
                ..Default::default()
            },
        )
        .with_cancel(cancel);

        let (tx, rx) = unbounded();
        agent.run_turn(&chat, "go", &tx).await.unwrap();
        drop(tx);
        let events: Vec<AgentEvent> = rx.collect().await;

        // Only the lifecycle bookends: started → cancelled, no model work between.
        assert!(matches!(
            events.first(),
            Some(AgentEvent::TurnStarted { .. })
        ));
        assert!(matches!(
            events.last(),
            Some(AgentEvent::TurnCancelled { .. })
        ));
        assert!(!events
            .iter()
            .any(|e| matches!(e, AgentEvent::TextDelta { .. })));
    }

    #[tokio::test]
    async fn oversized_transcript_emits_context_truncated() {
        let (store, chat, _workspace) = cancel_test_chat().await;

        // Records what the provider actually received, and answers immediately.
        struct AnswerProvider {
            seen_tokens: Arc<AtomicUsize>,
        }
        #[async_trait]
        impl ModelProvider for AnswerProvider {
            fn id(&self) -> ProviderId {
                ProviderId::new("answer")
            }
            async fn stream(&self, req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
                self.seen_tokens.store(
                    context::estimate_transcript_tokens(&req.messages),
                    Ordering::SeqCst,
                );
                Ok(stream::iter(vec![
                    ProviderEvent::TextDelta { text: "ok".into() },
                    ProviderEvent::Stop {
                        reason: StopReason::EndTurn,
                    },
                ])
                .boxed())
            }
        }

        let seen_tokens = Arc::new(AtomicUsize::new(0));
        // A small context window forces reduction of a large input.
        let context_window = 3000;
        let agent = Agent::new(
            Arc::new(AnswerProvider {
                seen_tokens: seen_tokens.clone(),
            }),
            Arc::new(ToolRegistry::new()),
            store,
            AgentConfig {
                model: "answer".into(),
                context_window,
                ..Default::default()
            },
        );

        let huge = "word ".repeat(2000); // ~3300 tokens, over the ~2250 budget
        let (tx, rx) = unbounded();
        agent.run_turn(&chat, &huge, &tx).await.unwrap();
        drop(tx);
        let events: Vec<AgentEvent> = rx.collect().await;

        let truncated = events.iter().find_map(|e| match e {
            AgentEvent::ContextTruncated {
                original_tokens,
                fitted_tokens,
            } => Some((*original_tokens, *fitted_tokens)),
            _ => None,
        });
        let (original, fitted) = truncated.expect("ContextTruncated emitted for oversized input");
        assert!(
            fitted < original,
            "fitted {fitted} should be < original {original}"
        );
        // What actually went to the provider matches the reported fitted size and
        // is within the reduced budget.
        assert_eq!(seen_tokens.load(Ordering::SeqCst), fitted as usize);
        assert!(fitted as usize <= context::compute_message_budget(context_window, 0, None, &[]));
    }

    #[tokio::test]
    async fn cancel_mid_stream_preempts_the_model_call() {
        let (store, chat, _workspace) = cancel_test_chat().await;

        let cancel = CancelToken::new();
        let agent = Agent::new(
            Arc::new(StallProvider),
            Arc::new(ToolRegistry::new()),
            store.clone(),
            AgentConfig {
                model: "stall".into(),
                ..Default::default()
            },
        )
        .with_cancel(cancel.clone());

        let (tx, mut rx) = unbounded();
        let chat_id = chat.id;
        let handle = tokio::spawn(async move {
            let _ = agent.run_turn(&chat, "go", &tx).await;
        });

        // Cancel the instant the first delta lands; the stream then stalls, so
        // only the cancel can end the turn.
        let mut cancelled = false;
        while let Some(event) = rx.next().await {
            match event {
                AgentEvent::TextDelta { text } if text == "partial" => cancel.cancel(),
                AgentEvent::TurnCancelled { .. } => cancelled = true,
                _ => {}
            }
        }
        handle.await.unwrap();

        assert!(cancelled, "a mid-stream cancel ends the turn as cancelled");
        // The partial assistant text of the preempted step is discarded, not stored.
        let roles: Vec<Role> = store
            .list_messages(chat_id)
            .await
            .unwrap()
            .iter()
            .map(|m| m.role)
            .collect();
        assert_eq!(roles, vec![Role::User]);
    }

    #[tokio::test]
    async fn cancel_drops_an_in_flight_server_tool_future() {
        let (store, chat, _workspace) = cancel_test_chat().await;
        let cancel = CancelToken::new();
        let entered = Arc::new(tokio::sync::Notify::new());
        let dropped = Arc::new(AtomicBool::new(false));
        let agent = Agent::new(
            Arc::new(BlockingToolProvider),
            Arc::new(ToolRegistry::new().with(Box::new(BlockingTool {
                entered: entered.clone(),
                dropped: dropped.clone(),
            }))),
            store,
            AgentConfig {
                model: "blocking-tool".into(),
                ..Default::default()
            },
        )
        .with_cancel(cancel.clone());

        let (tx, rx) = unbounded();
        let handle = tokio::spawn(async move {
            agent.run_turn(&chat, "go", &tx).await.unwrap();
        });

        entered.notified().await;
        cancel.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(1), handle)
            .await
            .expect("cancellation should stop an in-flight tool promptly")
            .unwrap();
        let events = rx.collect::<Vec<_>>().await;

        assert!(
            dropped.load(Ordering::SeqCst),
            "cancellation must drop the tool future so its HTTP request can abort"
        );
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::ToolCallCompleted { output, .. }
                if output.is_error && output.content == "turn cancelled during tool execution"
        )));
        assert!(matches!(
            events.last(),
            Some(AgentEvent::TurnCancelled { .. })
        ));
    }

    #[tokio::test]
    async fn cancel_unblocks_a_turn_parked_on_approval() {
        let (store, chat, _workspace) = cancel_test_chat().await;

        let (armed_tx, armed_rx) = futures::channel::oneshot::channel();
        let gate = Arc::new(SignalPendingGate {
            armed: std::sync::Mutex::new(Some(armed_tx)),
        });
        let ran = Arc::new(AtomicUsize::new(0));
        let cancel = CancelToken::new();
        let agent = Agent::new(
            Arc::new(BoomProvider {
                calls: AtomicUsize::new(0),
            }),
            Arc::new(ToolRegistry::new().with(Box::new(BoomTool { ran: ran.clone() }))),
            store,
            AgentConfig {
                model: "fake".into(),
                ..Default::default()
            },
        )
        .with_approvals(gate)
        .with_cancel(cancel.clone());

        let (tx, rx) = unbounded();
        let handle = tokio::spawn(async move {
            let _ = agent.run_turn(&chat, "go", &tx).await;
        });

        // Wait until the Sensitive call is genuinely parked, then cancel.
        armed_rx.await.unwrap();
        cancel.cancel();
        handle.await.unwrap();
        let events: Vec<AgentEvent> = rx.collect().await;

        assert_eq!(ran.load(Ordering::SeqCst), 0, "the parked tool never runs");
        assert!(events
            .iter()
            .any(|e| matches!(e, AgentEvent::ApprovalRequired { .. })));
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::ApprovalDecided {
                approved: false,
                ..
            }
        )));
        assert!(matches!(
            events.last(),
            Some(AgentEvent::TurnCancelled { .. })
        ));
    }

    #[tokio::test]
    async fn cancel_wins_when_approval_and_cancel_are_both_ready() {
        let (store, chat, _workspace) = cancel_test_chat().await;

        let ran = Arc::new(AtomicUsize::new(0));
        let cancel = CancelToken::new();
        let agent = Agent::new(
            Arc::new(BoomProvider {
                calls: AtomicUsize::new(0),
            }),
            Arc::new(ToolRegistry::new().with(Box::new(BoomTool { ran: ran.clone() }))),
            store,
            AgentConfig {
                model: "fake".into(),
                ..Default::default()
            },
        )
        .with_approvals(Arc::new(CancelThenApproveGate {
            cancel: cancel.clone(),
        }))
        .with_cancel(cancel);

        let (tx, rx) = unbounded();
        agent.run_turn(&chat, "go", &tx).await.unwrap();
        drop(tx);
        let events: Vec<AgentEvent> = rx.collect().await;

        assert_eq!(
            ran.load(Ordering::SeqCst),
            0,
            "cancel must preempt an approve that is ready in the same poll"
        );
        assert!(!events
            .iter()
            .any(|e| matches!(e, AgentEvent::ApprovalDecided { approved: true, .. })));
        assert!(matches!(
            events.last(),
            Some(AgentEvent::TurnCancelled { .. })
        ));
    }

    struct RestartTool {
        ran: Arc<AtomicUsize>,
        class: ApprovalClass,
    }

    #[async_trait]
    impl Tool for RestartTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "search".into(),
                description: "recover search".into(),
                input_schema: serde_json::json!({"type": "object"}),
            }
        }

        fn approval_class(&self) -> ApprovalClass {
            self.class
        }

        async fn execute(&self, _ctx: &ToolCtx, _args: Value) -> Result<ToolOutput> {
            self.ran.fetch_add(1, Ordering::SeqCst);
            Ok(ToolOutput::text("recovered result"))
        }
    }

    struct RestartGate(Arc<dyn Store>);

    impl ApprovalGate for RestartGate {
        fn register(
            &self,
            request: ApprovalRequest,
            _journal: Option<crate::approval::ApprovalJournalIdentity>,
        ) -> crate::approval::ApprovalRegistrationFuture<'_> {
            let store = self.0.clone();
            Box::pin(async move {
                let approval = store
                    .get_tool_call_approval(request.call_id)
                    .await
                    .unwrap()
                    .expect("approval receipt must survive restart");
                let decision = match approval.decision() {
                    Some(decision) => decision,
                    None => {
                        store
                            .decide_tool_call_approval(
                                request.chat_id,
                                request.call_id,
                                &ApprovalDecision::Approve,
                                Utc::now(),
                            )
                            .await
                            .unwrap();
                        ApprovalDecision::Approve
                    }
                };
                crate::approval::ApprovalRegistration {
                    decision: Box::pin(async move { decision }),
                    publication: crate::approval::ApprovalRequiredPublication::None,
                }
            })
        }
    }

    struct RestartProvider {
        provider_id: String,
        expect_error: bool,
    }

    #[async_trait]
    impl ModelProvider for RestartProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("restart")
        }

        async fn stream(&self, request: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            assert!(request.messages.iter().any(|message| {
                message.content.iter().any(|block| {
                    matches!(
                        block,
                    ContentBlock::ToolResult { tool_use_id, is_error, .. }
                        if tool_use_id == &self.provider_id && *is_error == self.expect_error
                    )
                })
            }));
            Ok(stream::iter(vec![
                ProviderEvent::TextDelta {
                    text: "done".into(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ])
            .boxed())
        }
    }

    async fn assert_sensitive_restart_recovery(
        preapproved: bool,
        current_class: ApprovalClass,
        tool_present: bool,
    ) {
        let db = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                db.path().join("restart.db").display()
            ))
            .await
            .unwrap(),
        );
        let chat = Chat {
            id: ChatId::new(),
            project_id: None,
            title: None,
            model: None,
            reasoning_effort: None,
            attachment_revision: 0,
            root_attachments: Vec::new(),
            created_at: Utc::now(),
        };
        store.create_chat(&chat).await.unwrap();
        let turn_id = TurnId::new();
        let accepted = match store
            .accept_turn(turn_id, chat.id, "fake", "search")
            .await
            .unwrap()
        {
            crate::storage::AcceptTurnOutcome::Accepted(turn) => turn,
            outcome => panic!("unexpected turn acceptance: {outcome:?}"),
        };
        let lease_token = uuid::Uuid::new_v4();
        let claimed_at = Utc::now().max(accepted.available_at);
        store
            .claim_turn_run(
                lease_token,
                claimed_at,
                claimed_at + chrono::Duration::minutes(5),
            )
            .await
            .unwrap();
        let call = ToolCallRecord {
            id: CallId::new(),
            chat_id: chat.id,
            turn_id,
            provider_id: "persisted-search".into(),
            name: "search".into(),
            arguments: serde_json::json!({"query": "restart"}),
            execution: ToolCallExecution::Server,
            status: ToolCallStatus::Pending,
            result: None,
            error_code: None,
            error_detail: None,
            client_executor_id: None,
            client_lease_expires_at: None,
            created_at: claimed_at,
            resolved_at: None,
        };
        assert!(matches!(
            store
                .accept_claimed_tool_call(&call, lease_token, claimed_at)
                .await
                .unwrap(),
            AcceptClaimedToolCallOutcome::Accepted(_)
        ));
        store
            .request_tool_call_approval(
                &ApprovalRequest {
                    call_id: call.id,
                    chat_id: chat.id,
                    turn_id,
                    tool_name: call.name.clone(),
                    class: ApprovalClass::Sensitive,
                    kind: ToolApprovalKind::for_tool_name(&call.name),
                    preview: None,
                    summary: "search requires approval".into(),
                },
                Utc::now(),
            )
            .await
            .unwrap();
        if preapproved {
            store
                .decide_tool_call_approval(chat.id, call.id, &ApprovalDecision::Approve, Utc::now())
                .await
                .unwrap();
        }
        let ran = Arc::new(AtomicUsize::new(0));
        let mut registry = ToolRegistry::new();
        if tool_present {
            registry.register(Box::new(RestartTool {
                ran: ran.clone(),
                class: current_class,
            }));
        }
        let agent = Agent::new(
            Arc::new(RestartProvider {
                provider_id: call.provider_id.clone(),
                expect_error: true,
            }),
            Arc::new(registry),
            store.clone(),
            AgentConfig {
                model: "fake".into(),
                ..AgentConfig::default()
            },
        )
        .with_approvals(Arc::new(RestartGate(store.clone())))
        .with_durable_steer(lease_token);
        let (tx, mut rx) = unbounded();
        let events = tokio::spawn(async move {
            let mut collected = Vec::new();
            while let Some(event) = rx.next().await {
                match event {
                    ClaimedAgentEvent::Flush(acknowledge) => {
                        let _ = acknowledge.send(());
                    }
                    ClaimedAgentEvent::Pending { event, .. } => collected.push(event),
                    ClaimedAgentEvent::Committed { event, .. }
                    | ClaimedAgentEvent::Recovered { event, .. } => {
                        collected.push(event.event);
                    }
                }
            }
            collected
        });
        assert!(matches!(
            agent
                .run_claimed_turn(&chat, turn_id, MessageId::new(), 1, &tx)
                .await
                .unwrap(),
            AgentTurnOutcome::Completed { .. }
        ));
        drop(tx);
        let events = events.await.unwrap();
        assert_eq!(ran.load(Ordering::SeqCst), 0);
        assert_eq!(
            store
                .list_tool_calls(chat.id)
                .await
                .unwrap()
                .into_iter()
                .find(|stored| stored.id == call.id)
                .unwrap()
                .status,
            ToolCallStatus::Failed
        );
        let approval_decided = events
            .iter()
            .position(|event| {
                matches!(
                    event,
                    AgentEvent::ApprovalDecided { call_id, .. } if *call_id == call.id
                )
            })
            .expect("recovery must close its durable approval card");
        let tool_completed = events
            .iter()
            .position(|event| {
                matches!(
                    event,
                    AgentEvent::ToolCallCompleted { call_id, .. } if *call_id == call.id
                )
            })
            .expect("recovery must publish its failed completion");
        assert!(approval_decided < tool_completed);
    }

    #[tokio::test]
    async fn reclaimed_turn_suppresses_pending_and_preapproved_sensitive_calls() {
        assert_sensitive_restart_recovery(false, ApprovalClass::ReadOnly, true).await;
        assert_sensitive_restart_recovery(true, ApprovalClass::Sensitive, true).await;
        assert_sensitive_restart_recovery(false, ApprovalClass::ReadOnly, false).await;
    }

    async fn pending_workspace_restart(
        name: &str,
        arguments: Value,
    ) -> (
        tempfile::TempDir,
        Arc<dyn Store>,
        Chat,
        TurnId,
        uuid::Uuid,
        ToolCallRecord,
    ) {
        let db = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                db.path().join("cancelled-restart.db").display()
            ))
            .await
            .unwrap(),
        );
        let chat = Chat {
            id: ChatId::new(),
            project_id: None,
            title: None,
            model: None,
            reasoning_effort: None,
            attachment_revision: 0,
            root_attachments: Vec::new(),
            created_at: Utc::now(),
        };
        store.create_chat(&chat).await.unwrap();
        let turn_id = TurnId::new();
        let accepted = match store
            .accept_turn(turn_id, chat.id, "fake", "recover workspace call")
            .await
            .unwrap()
        {
            crate::storage::AcceptTurnOutcome::Accepted(turn) => turn,
            outcome => panic!("unexpected turn acceptance: {outcome:?}"),
        };
        let lease_token = uuid::Uuid::new_v4();
        let claimed_at = Utc::now().max(accepted.available_at);
        assert!(store
            .claim_turn_run(
                lease_token,
                claimed_at,
                claimed_at + chrono::Duration::minutes(5),
            )
            .await
            .unwrap()
            .turn
            .is_some());
        let call = ToolCallRecord {
            id: CallId::new(),
            chat_id: chat.id,
            turn_id,
            provider_id: "persisted-workspace-call".into(),
            name: name.into(),
            arguments,
            execution: ToolCallExecution::Server,
            status: ToolCallStatus::Pending,
            result: None,
            error_code: None,
            error_detail: None,
            client_executor_id: None,
            client_lease_expires_at: None,
            created_at: claimed_at,
            resolved_at: None,
        };
        assert!(matches!(
            store
                .accept_claimed_tool_call(&call, lease_token, claimed_at)
                .await
                .unwrap(),
            AcceptClaimedToolCallOutcome::Accepted(_)
        ));
        (db, store, chat, turn_id, lease_token, call)
    }

    #[tokio::test]
    async fn cancelled_reclaim_resolves_pending_write_without_touching_scratch() {
        let scratch = tempfile::tempdir().unwrap();
        let (_db, store, chat, turn_id, lease_token, call) = pending_workspace_restart(
            "write_file",
            serde_json::json!({"path": "cancelled.txt", "content": "must not exist"}),
        )
        .await;
        store
            .request_tool_call_approval(
                &ApprovalRequest {
                    call_id: call.id,
                    chat_id: chat.id,
                    turn_id,
                    tool_name: call.name.clone(),
                    class: ApprovalClass::Sensitive,
                    kind: ToolApprovalKind::for_tool_name(&call.name),
                    preview: None,
                    summary: "persisted write requires approval".into(),
                },
                Utc::now(),
            )
            .await
            .unwrap();
        let cancel = CancelToken::new();
        cancel.cancel();
        let provider = Arc::new(BoomProvider {
            calls: AtomicUsize::new(0),
        });
        let agent = Agent::new(
            provider.clone(),
            Arc::new(ToolRegistry::new().with(Box::new(WriteFile))),
            store.clone(),
            AgentConfig {
                model: "fake".into(),
                tool_scratch: Some(tool_scratch(scratch.path())),
                ..AgentConfig::default()
            },
        )
        .with_cancel(cancel)
        .with_durable_steer(lease_token);
        let (tx, rx) = unbounded();
        assert!(matches!(
            agent
                .run_claimed_turn(&chat, turn_id, MessageId::new(), 1, &tx)
                .await
                .unwrap(),
            AgentTurnOutcome::Cancelled { .. }
        ));
        drop(tx);
        let events = emitted_events(rx.collect().await);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        assert!(!scratch.path().join("cancelled.txt").exists());
        let approval_decided = events
            .iter()
            .position(|event| {
                matches!(
                    event,
                    AgentEvent::ApprovalDecided {
                        call_id,
                        approved: false,
                    } if *call_id == call.id
                )
            })
            .expect("cancelled recovery must close its durable approval card");
        let tool_completed = events
            .iter()
            .position(|event| {
                matches!(event, AgentEvent::ToolCallCompleted { call_id, .. } if *call_id == call.id)
            })
            .expect("cancelled recovery must publish failed tool completion");
        assert!(approval_decided < tool_completed);
        assert_eq!(
            store
                .list_tool_calls(chat.id)
                .await
                .unwrap()
                .into_iter()
                .find(|stored| stored.id == call.id)
                .unwrap()
                .status,
            ToolCallStatus::Failed
        );
    }

    struct CancelDuringRecoveryTool {
        cancel: CancelToken,
        classifications: AtomicUsize,
        ran: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Tool for CancelDuringRecoveryTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "recovery_write".into(),
                description: "test recovery cancellation fence".into(),
                input_schema: serde_json::json!({"type": "object"}),
            }
        }

        fn approval_class(&self) -> ApprovalClass {
            if self.classifications.fetch_add(1, Ordering::SeqCst) == 1 {
                self.cancel.cancel();
            }
            ApprovalClass::Workspace
        }

        async fn execute(&self, _ctx: &ToolCtx, _args: Value) -> Result<ToolOutput> {
            self.ran.fetch_add(1, Ordering::SeqCst);
            Ok(ToolOutput::text("unexpected execution"))
        }
    }

    #[tokio::test]
    async fn recovery_never_reexecutes_a_pending_workspace_call() {
        let (_db, store, chat, turn_id, lease_token, call) =
            pending_workspace_restart("recovery_write", serde_json::json!({})).await;
        let cancel = CancelToken::new();
        let ran = Arc::new(AtomicUsize::new(0));
        let tool = CancelDuringRecoveryTool {
            cancel: cancel.clone(),
            classifications: AtomicUsize::new(0),
            ran: ran.clone(),
        };
        let agent = Agent::new(
            Arc::new(BoomProvider {
                calls: AtomicUsize::new(0),
            }),
            Arc::new(ToolRegistry::new().with(Box::new(tool))),
            store.clone(),
            AgentConfig {
                model: "fake".into(),
                ..AgentConfig::default()
            },
        )
        .with_cancel(cancel)
        .with_durable_steer(lease_token);
        let (tx, _rx) = unbounded();
        assert!(matches!(
            agent
                .run_claimed_turn(&chat, turn_id, MessageId::new(), 1, &tx)
                .await
                .unwrap(),
            AgentTurnOutcome::Completed { .. }
        ));
        assert_eq!(ran.load(Ordering::SeqCst), 0);
        assert_eq!(
            store
                .list_tool_calls(chat.id)
                .await
                .unwrap()
                .into_iter()
                .find(|stored| stored.id == call.id)
                .unwrap()
                .status,
            ToolCallStatus::Failed
        );
    }

    #[tokio::test]
    async fn interrupt_steer_preempts_mid_stream_and_continues() {
        let (store, chat, _workspace) = cancel_test_chat().await;

        // First call stalls after "partial"; after steer, second call finishes.
        struct StallThenFinish {
            calls: AtomicUsize,
        }
        #[async_trait]
        impl ModelProvider for StallThenFinish {
            fn id(&self) -> ProviderId {
                ProviderId::new("stall-then-finish")
            }
            async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
                if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    let head = stream::iter(vec![ProviderEvent::TextDelta {
                        text: "partial".into(),
                    }]);
                    return Ok(head.chain(stream::pending()).boxed());
                }
                Ok(stream::iter(vec![
                    ProviderEvent::TextDelta {
                        text: "after steer".into(),
                    },
                    ProviderEvent::Stop {
                        reason: StopReason::EndTurn,
                    },
                ])
                .boxed())
            }
        }

        let steer = SteerInbox::new();
        let agent = Agent::new(
            Arc::new(StallThenFinish {
                calls: AtomicUsize::new(0),
            }),
            Arc::new(ToolRegistry::new()),
            store.clone(),
            AgentConfig {
                model: "stall".into(),
                ..Default::default()
            },
        )
        .with_steer(steer.clone());

        let chat_id = chat.id;
        let (tx, mut rx) = unbounded();
        let handle = tokio::spawn(async move {
            let _ = agent.run_turn(&chat, "go", &tx).await;
        });

        let mut steered = false;
        let mut interrupted = false;
        let mut completed = false;
        while let Some(event) = rx.next().await {
            match event {
                AgentEvent::TextDelta { text } if text == "partial" => {
                    steer.push("please change course", true);
                }
                AgentEvent::StreamInterrupted => {
                    interrupted = true;
                }
                AgentEvent::UserSteered { content, .. } => {
                    assert_eq!(content, "please change course");
                    steered = true;
                }
                AgentEvent::TurnCompleted { .. } => completed = true,
                AgentEvent::TurnCancelled { .. } => {
                    panic!("steer must continue the turn, not cancel it")
                }
                _ => {}
            }
        }
        handle.await.unwrap();

        assert!(
            interrupted,
            "interrupt steer marks the partial provider stream as abandoned"
        );
        assert!(steered, "steer event emitted");
        assert!(completed, "turn completes after steer");
        let roles: Vec<_> = store
            .list_messages(chat_id)
            .await
            .unwrap()
            .iter()
            .map(|m| (m.role, m.content.clone()))
            .collect();
        // Initial user + steered user + final assistant (partial discarded).
        assert!(roles.iter().any(|(r, c)| *r == Role::User && c == "go"));
        assert!(roles
            .iter()
            .any(|(r, c)| *r == Role::User && c == "please change course"));
        assert!(roles
            .iter()
            .any(|(r, c)| *r == Role::Assistant && c == "after steer"));
        assert!(!roles.iter().any(|(_, c)| c == "partial"));
    }

    #[tokio::test]
    async fn boundary_steer_persists_distinct_legacy_assistant_candidates() {
        let (store, chat, _workspace) = cancel_test_chat().await;

        struct BoundaryThenFinish {
            calls: AtomicUsize,
            release: Mutex<Option<futures::channel::oneshot::Receiver<()>>>,
        }
        #[async_trait]
        impl ModelProvider for BoundaryThenFinish {
            fn id(&self) -> ProviderId {
                ProviderId::new("boundary-then-finish")
            }

            async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
                if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    let release = self.release.lock().unwrap().take().unwrap();
                    return Ok(stream::iter(vec![ProviderEvent::TextDelta {
                        text: "first candidate".into(),
                    }])
                    .chain(stream::once(async move {
                        let _ = release.await;
                        ProviderEvent::Stop {
                            reason: StopReason::EndTurn,
                        }
                    }))
                    .boxed());
                }
                Ok(stream::iter(vec![
                    ProviderEvent::TextDelta {
                        text: "final candidate".into(),
                    },
                    ProviderEvent::Stop {
                        reason: StopReason::EndTurn,
                    },
                ])
                .boxed())
            }
        }

        let (release_tx, release_rx) = futures::channel::oneshot::channel();
        let steer = SteerInbox::new();
        let agent = Agent::new(
            Arc::new(BoundaryThenFinish {
                calls: AtomicUsize::new(0),
                release: Mutex::new(Some(release_rx)),
            }),
            Arc::new(ToolRegistry::new()),
            store.clone(),
            AgentConfig {
                model: "fake".into(),
                ..Default::default()
            },
        )
        .with_steer(steer.clone());

        let chat_id = chat.id;
        let (tx, mut rx) = unbounded();
        let run = tokio::spawn(async move { agent.run_turn(&chat, "go", &tx).await });
        while let Some(event) = rx.next().await {
            if matches!(
                event,
                AgentEvent::TextDelta { ref text } if text == "first candidate"
            ) {
                assert!(steer.push("revise that", false));
                let _ = release_tx.send(());
                break;
            }
        }
        run.await.unwrap().unwrap();

        let messages = store.list_messages(chat_id).await.unwrap();
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0].content, "go");
        assert_eq!(messages[1].content, "first candidate");
        assert_eq!(messages[2].content, "revise that");
        assert_eq!(messages[3].content, "final candidate");
        assert_ne!(messages[1].id, messages[3].id);
    }

    #[tokio::test]
    async fn cancel_wins_over_steer_when_both_ready() {
        let (store, chat, _workspace) = cancel_test_chat().await;

        let cancel = CancelToken::new();
        let steer = SteerInbox::new();
        // Trip both before the turn starts racing the stream.
        cancel.cancel();
        steer.push("ignored", true);

        let agent = Agent::new(
            Arc::new(StallProvider),
            Arc::new(ToolRegistry::new()),
            store,
            AgentConfig {
                model: "stall".into(),
                ..Default::default()
            },
        )
        .with_cancel(cancel)
        .with_steer(steer);

        let (tx, rx) = unbounded();
        agent.run_turn(&chat, "go", &tx).await.unwrap();
        drop(tx);
        let events: Vec<AgentEvent> = rx.collect().await;

        assert!(matches!(
            events.last(),
            Some(AgentEvent::TurnCancelled { .. })
        ));
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AgentEvent::UserSteered { .. })),
            "cancel must win; steer is not applied"
        );
    }

    #[tokio::test]
    async fn sensitive_tool_is_refused_without_a_gate() {
        let db = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                db.path().join("t.db").display()
            ))
            .await
            .unwrap(),
        );
        let chat = Chat {
            id: ChatId::new(),
            project_id: None,
            title: None,
            model: None,
            reasoning_effort: None,
            attachment_revision: 0,
            root_attachments: Vec::new(),
            created_at: Utc::now(),
        };
        store.create_chat(&chat).await.unwrap();

        let ran = Arc::new(AtomicUsize::new(0));
        let agent = Agent::new(
            Arc::new(BoomProvider {
                calls: AtomicUsize::new(0),
            }),
            Arc::new(ToolRegistry::new().with(Box::new(BoomTool { ran: ran.clone() }))),
            store,
            AgentConfig {
                model: "fake".into(),
                ..Default::default()
            },
        );

        let (tx, rx) = unbounded();
        agent.run_turn(&chat, "go", &tx).await.unwrap();
        drop(tx);
        let events: Vec<AgentEvent> = rx.collect().await;

        assert_eq!(ran.load(Ordering::SeqCst), 0);
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::ApprovalDecided {
                approved: false,
                ..
            }
        )));
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::ToolCallCompleted { output, .. } if output.is_error
        )));
    }

    #[test]
    fn rebuild_attaches_tools_to_assistant_text() {
        let turn = TurnId::new();
        let chat = ChatId::new();
        let t0 = DateTime::<Utc>::from_timestamp(1_000, 0).unwrap();
        let t1 = DateTime::<Utc>::from_timestamp(1_001, 0).unwrap();
        let t2 = DateTime::<Utc>::from_timestamp(1_002, 0).unwrap();
        let messages = vec![
            Message {
                id: MessageId::new(),
                chat_id: chat,
                turn_id: turn,
                role: Role::User,
                content: "read it".into(),
                created_at: t0,
            },
            Message {
                id: MessageId::new(),
                chat_id: chat,
                turn_id: turn,
                role: Role::Assistant,
                content: "looking…".into(),
                created_at: t1,
            },
        ];
        let calls = vec![ToolCallRecord {
            id: CallId::new(),
            chat_id: chat,
            turn_id: turn,
            provider_id: "tu_1".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({"path": "a"}),
            execution: ToolCallExecution::Server,
            status: ToolCallStatus::Completed,
            result: Some("ok".into()),
            error_code: None,
            error_detail: None,
            client_executor_id: None,
            client_lease_expires_at: None,
            created_at: t2,
            resolved_at: Some(DateTime::<Utc>::from_timestamp(1_003, 0).unwrap()),
        }];
        let rebuilt = rebuild_transcript(&messages, &calls);
        assert_eq!(rebuilt.len(), 3);
        assert_eq!(rebuilt[0].role, Role::User);
        assert!(matches!(
            &rebuilt[1].content[..],
            [
                ContentBlock::Text { text },
                ContentBlock::ToolUse { id, name, .. }
            ] if text == "looking…" && id == "tu_1" && name == "read_file"
        ));
        assert!(matches!(
            &rebuilt[2].content[..],
            [ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error: false
            }] if tool_use_id == "tu_1" && content == "ok"
        ));
    }

    #[test]
    fn orchestration_forces_a_model_step_boundary_despite_overlapping_timestamps() {
        let turn = TurnId::new();
        let chat = ChatId::new();
        let t1 = DateTime::<Utc>::from_timestamp(1_001, 0).unwrap();
        let t2 = DateTime::<Utc>::from_timestamp(1_002, 0).unwrap();
        let t3 = DateTime::<Utc>::from_timestamp(1_003, 0).unwrap();
        let call = |provider_id: &str,
                    execution: ToolCallExecution,
                    created_at: DateTime<Utc>,
                    resolved_at: DateTime<Utc>| ToolCallRecord {
            id: CallId::new(),
            chat_id: chat,
            turn_id: turn,
            provider_id: provider_id.into(),
            name: if execution == ToolCallExecution::Orchestration {
                crate::SPAWN_SANDBOX_AGENT_TOOL.into()
            } else {
                "read_file".into()
            },
            arguments: serde_json::json!({}),
            execution,
            status: ToolCallStatus::Completed,
            result: Some("ok".into()),
            error_code: None,
            error_detail: None,
            client_executor_id: None,
            client_lease_expires_at: None,
            created_at,
            resolved_at: Some(resolved_at),
        };
        let calls = vec![
            call("ordinary-before", ToolCallExecution::Server, t1, t3),
            call("spawn", ToolCallExecution::Orchestration, t2, t2),
            call("ordinary-after", ToolCallExecution::Server, t2, t3),
        ];
        let batches = batch_tool_calls(&calls);
        assert_eq!(batches.len(), 3);
        assert_eq!(
            batches
                .iter()
                .map(|batch| batch
                    .iter()
                    .map(|call| call.provider_id.as_str())
                    .collect::<Vec<_>>())
                .collect::<Vec<_>>(),
            vec![
                vec!["ordinary-before"],
                vec!["spawn"],
                vec!["ordinary-after"],
            ]
        );
    }

    #[test]
    fn answered_user_questions_rebuild_as_a_model_facing_tool_result() {
        let turn = TurnId::new();
        let chat = ChatId::new();
        let created_at = DateTime::<Utc>::from_timestamp(1_001, 0).unwrap();
        let answer = crate::AnswerUserQuestions {
            answers: vec![crate::UserQuestionAnswer {
                question_id: "target".into(),
                option_id: Some("staging".into()),
                free_form: None,
            }],
        };
        let calls = vec![ToolCallRecord {
            id: CallId::new(),
            chat_id: chat,
            turn_id: turn,
            provider_id: "question_1".into(),
            name: crate::ASK_USER_QUESTIONS_TOOL.into(),
            arguments: serde_json::json!({
                "questions": [{
                    "id": "target",
                    "header": "Target",
                    "question": "Where should I deploy?",
                    "options": [{
                        "id": "staging",
                        "label": "Staging",
                        "description": "Deploy for verification."
                    }]
                }]
            }),
            execution: ToolCallExecution::Orchestration,
            status: ToolCallStatus::Completed,
            result: Some(serde_json::to_string(&answer).unwrap()),
            error_code: None,
            error_detail: None,
            client_executor_id: None,
            client_lease_expires_at: None,
            created_at,
            resolved_at: Some(created_at),
        }];

        let rebuilt = rebuild_transcript(&[], &calls);
        assert_eq!(rebuilt.len(), 2);
        assert!(matches!(
            &rebuilt[0],
            ChatMessage {
                role: Role::Assistant,
                content: assistant,
            } if matches!(
                &assistant[..],
                [ContentBlock::ToolUse { id, name, .. }]
                    if id == "question_1" && name == crate::ASK_USER_QUESTIONS_TOOL
            )
        ));
        let ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
        } = &rebuilt[1].content[0]
        else {
            panic!("answer must rebuild as a tool result");
        };
        assert_eq!(rebuilt[1].role, Role::User);
        assert_eq!(tool_use_id, "question_1");
        assert!(!is_error);
        assert_eq!(
            serde_json::from_str::<crate::AnswerUserQuestions>(content).unwrap(),
            answer
        );
    }

    #[test]
    fn rebuild_emits_tool_only_step_before_final_text() {
        let turn = TurnId::new();
        let chat = ChatId::new();
        let t0 = DateTime::<Utc>::from_timestamp(1_000, 0).unwrap();
        let t1 = DateTime::<Utc>::from_timestamp(1_001, 0).unwrap();
        let t2 = DateTime::<Utc>::from_timestamp(1_002, 0).unwrap();
        let messages = vec![
            Message {
                id: MessageId::new(),
                chat_id: chat,
                turn_id: turn,
                role: Role::User,
                content: "go".into(),
                created_at: t0,
            },
            Message {
                id: MessageId::new(),
                chat_id: chat,
                turn_id: turn,
                role: Role::Assistant,
                content: "done".into(),
                created_at: t2,
            },
        ];
        let calls = vec![ToolCallRecord {
            id: CallId::new(),
            chat_id: chat,
            turn_id: turn,
            provider_id: "tu_1".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({}),
            execution: ToolCallExecution::Server,
            status: ToolCallStatus::Completed,
            result: Some("data".into()),
            error_code: None,
            error_detail: None,
            client_executor_id: None,
            client_lease_expires_at: None,
            created_at: t1,
            resolved_at: Some(t1),
        }];
        let rebuilt = rebuild_transcript(&messages, &calls);
        assert_eq!(rebuilt.len(), 4);
        assert_eq!(rebuilt[0].role, Role::User);
        assert!(matches!(
            &rebuilt[1].content[..],
            [ContentBlock::ToolUse { .. }]
        ));
        assert!(matches!(
            &rebuilt[2].content[..],
            [ContentBlock::ToolResult { .. }]
        ));
        assert_eq!(rebuilt[3].role, Role::Assistant);
    }

    #[test]
    fn rebuild_skips_legacy_tool_role_rows() {
        let turn = TurnId::new();
        let chat = ChatId::new();
        let messages = vec![
            Message {
                id: MessageId::new(),
                chat_id: chat,
                turn_id: turn,
                role: Role::User,
                content: "hi".into(),
                created_at: DateTime::<Utc>::from_timestamp(1, 0).unwrap(),
            },
            Message {
                id: MessageId::new(),
                chat_id: chat,
                turn_id: turn,
                role: Role::Tool,
                content: "legacy".into(),
                created_at: DateTime::<Utc>::from_timestamp(2, 0).unwrap(),
            },
            Message {
                id: MessageId::new(),
                chat_id: chat,
                turn_id: turn,
                role: Role::Assistant,
                content: "bye".into(),
                created_at: DateTime::<Utc>::from_timestamp(3, 0).unwrap(),
            },
        ];
        let rebuilt = rebuild_transcript(&messages, &[]);
        assert_eq!(rebuilt.len(), 2);
        assert_eq!(rebuilt[0].role, Role::User);
        assert_eq!(rebuilt[1].role, Role::Assistant);
    }

    #[tokio::test]
    async fn second_turn_rebuilds_prior_tool_calls_into_transcript() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("note.txt"), "hello from disk").unwrap();
        let db = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                db.path().join("t.db").display()
            ))
            .await
            .unwrap(),
        );
        let chat = Chat {
            id: ChatId::new(),
            project_id: None,
            title: None,
            model: None,
            reasoning_effort: None,
            attachment_revision: 0,
            root_attachments: Vec::new(),
            created_at: Utc::now(),
        };
        store.create_chat(&chat).await.unwrap();

        // Turn 1: tool call then finish (FakeProvider).
        let agent = Agent::new(
            Arc::new(FakeProvider {
                calls: AtomicUsize::new(0),
            }),
            Arc::new(ToolRegistry::new().with(Box::new(ReadFile))),
            store.clone(),
            AgentConfig {
                model: "fake".into(),
                tool_scratch: Some(tool_scratch(workspace.path())),
                ..Default::default()
            },
        );
        let (tx, rx) = unbounded();
        agent.run_turn(&chat, "read note.txt", &tx).await.unwrap();
        drop(tx);
        let _: Vec<AgentEvent> = rx.collect().await;

        // Turn 2: provider that records the request so we can assert ToolUse/Result
        // blocks were rebuilt from the store.
        let seen: Arc<Mutex<Vec<ChatMessage>>> = Arc::new(Mutex::new(Vec::new()));
        struct CaptureProvider {
            seen: Arc<Mutex<Vec<ChatMessage>>>,
        }
        #[async_trait]
        impl ModelProvider for CaptureProvider {
            fn id(&self) -> ProviderId {
                ProviderId::new("capture")
            }
            async fn stream(&self, req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
                *self.seen.lock().unwrap() = req.messages;
                Ok(stream::iter(vec![
                    ProviderEvent::TextDelta { text: "ok".into() },
                    ProviderEvent::Stop {
                        reason: StopReason::EndTurn,
                    },
                ])
                .boxed())
            }
        }
        let agent = Agent::new(
            Arc::new(CaptureProvider { seen: seen.clone() }),
            Arc::new(ToolRegistry::new()),
            store,
            AgentConfig {
                model: "fake".into(),
                ..Default::default()
            },
        );
        let (tx, rx) = unbounded();
        agent
            .run_turn(&chat, "what did you find?", &tx)
            .await
            .unwrap();
        drop(tx);
        let _: Vec<AgentEvent> = rx.collect().await;

        let messages = seen.lock().unwrap().clone();
        assert!(
            messages.iter().any(|m| {
                m.role == Role::Assistant
                    && m.content.iter().any(
                        |b| matches!(b, ContentBlock::ToolUse { name, .. } if name == "read_file"),
                    )
            }),
            "expected rebuilt ToolUse in cross-turn transcript: {messages:?}"
        );
        assert!(
            messages.iter().any(|m| {
                m.role == Role::User
                    && m.content.iter().any(|b| {
                        matches!(
                            b,
                            ContentBlock::ToolResult { content, .. } if content == "hello from disk"
                        )
                    })
            }),
            "expected rebuilt ToolResult in cross-turn transcript: {messages:?}"
        );
    }
}
