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
//!   [`ApprovalGate`] until approve/reject (standing grants / auto-judge later);
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

use crate::approval::{ApprovalDecision, ApprovalGate, ApprovalRequest, RefuseGate};
use crate::cancel::CancelToken;
use crate::context;
use crate::error::{AgentError, Result};
use crate::event::{AgentEvent, SequencedEvent};
use crate::id::{CallId, MessageId, TurnId};
use crate::model::{
    Chat, Message, Role, ToolCallExecution, ToolCallRecord, ToolCallResolution, ToolCallStatus,
    TurnRunStatus,
};
use crate::provider::{
    ChatMessage, ChatRequest, ContentBlock, ModelProvider, ProviderEvent, StopReason, Usage,
};
use crate::steer::SteerInbox;
use crate::storage::{
    AcceptToolCallOutcome, ApplyTurnSteerOutcome, JournaledTurnSteerOutcome,
    ResolveToolCallOutcome, Store,
};
use crate::tool::{ApprovalClass, Tool, ToolCtx, ToolOutput, ToolScratch, ToolSpec};

/// A name-keyed registry of the tools available to the agent.
#[derive(Default)]
pub struct ToolRegistry {
    tools: HashMap<String, RegisteredTool>,
}

enum RegisteredTool {
    Server(Box<dyn Tool>),
    Client {
        spec: ToolSpec,
        validate_arguments: Option<fn(&Value) -> bool>,
    },
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
            .insert(tool.spec().name, RegisteredTool::Server(tool));
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
            Some(RegisteredTool::Client { .. }) | None => None,
        }
    }

    /// Resolve the trusted execution surface for a registered tool name.
    #[must_use]
    pub fn execution(&self, name: &str) -> Option<ToolCallExecution> {
        self.tools.get(name).map(|tool| match tool {
            RegisteredTool::Server(_) => ToolCallExecution::Server,
            RegisteredTool::Client { .. } => ToolCallExecution::Client,
        })
    }

    /// The specs of every registered tool, to advertise to the model.
    #[must_use]
    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools
            .values()
            .map(|tool| match tool {
                RegisteredTool::Server(tool) => tool.spec(),
                RegisteredTool::Client { spec, .. } => spec.clone(),
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
            Some(RegisteredTool::Server(_)) | None => false,
        }
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
    /// Provider model identifier (e.g. `claude-opus-4-8`).
    pub model: String,
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
            model: String::new(),
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
    cancel: CancelToken,
    steer: SteerInbox,
    durable_steer_lease: Option<uuid::Uuid>,
}

/// A tool call accumulated from the provider stream.
struct PendingCall {
    call_id: CallId,
    provider_id: String,
    name: String,
    args: String,
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
            cancel: CancelToken::new(),
            steer: SteerInbox::new(),
            durable_steer_lease: None,
        }
    }

    /// Use `gate` for Sensitive-tool decisions (park-and-resume on the server).
    #[must_use]
    pub fn with_approvals(mut self, gate: Arc<dyn ApprovalGate>) -> Self {
        self.approvals = gate;
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
        let mut total_usage = Usage::default();
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
                    model: self.config.model.clone(),
                    system: self.config.system_prompt.clone(),
                    messages: fitted,
                    tools: self.tools.specs(),
                    max_tokens: self.config.max_tokens,
                    temperature: self.config.temperature,
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
            }
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
                        events.send(AgentEvent::TextDelta {
                            text: delta.clone(),
                        });
                        text.push_str(&delta);
                    }
                    ProviderEvent::ReasoningDelta { text: delta } => {
                        events.send(AgentEvent::ReasoningDelta { text: delta });
                    }
                    ProviderEvent::ToolCallStarted { index, id, name } => {
                        let call_id = CallId::new();
                        events.send(AgentEvent::ToolCallStarted {
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
                            events.send(AgentEvent::ToolCallArgsDelta {
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
                }
            };
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

            let client_calls = calls
                .iter()
                .filter(|call| self.tools.execution(&call.name) == Some(ToolCallExecution::Client))
                .collect::<Vec<_>>();
            if !client_calls.is_empty() {
                if calls.len() != 1 || client_calls.len() != 1 || !text.is_empty() {
                    events.send(AgentEvent::StreamInterrupted);
                    transcript.push(ChatMessage {
                        role: Role::User,
                        content: vec![ContentBlock::Text {
                            text: "A client-executed tool must be requested alone, without assistant text or sibling tool calls. Retry the request in that form.".into(),
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
                return Ok(AgentTurnOutcome::ClientToolCall {
                    request,
                    usage: total_usage,
                    steer_revision,
                    model_steps: step + 1,
                });
            }

            // Record the assistant message (text + any tool-use blocks).
            let mut blocks: Vec<ContentBlock> = Vec::new();
            if !text.is_empty() {
                blocks.push(ContentBlock::Text { text: text.clone() });
                if !calls.is_empty() {
                    self.persist(chat.id, turn_id, Role::Assistant, &text)
                        .await?;
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
                let outcome = self
                    .store
                    .accept_tool_call(&ToolCallRecord {
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
                    })
                    .await?;
                match outcome {
                    AcceptToolCallOutcome::Accepted(_) => {}
                    AcceptToolCallOutcome::Existing(existing) if existing.status.is_terminal() => {
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
                            },
                        );
                    }
                    AcceptToolCallOutcome::Existing(_) => {}
                    AcceptToolCallOutcome::IdentityConflict => {
                        return Err(AgentError::Store(format!(
                            "tool call {} identity conflicts with its canonical request",
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
                    self.store.append_message(&output).await?;
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
                            (!publish_terminal && !text.is_empty()).then_some(text.as_str()),
                            events,
                        )
                        .await?
                    {
                        break; // continue the outer step loop below
                    }
                    if self.durable_steer_lease.is_some() {
                        return Ok(AgentTurnOutcome::Completed {
                            output,
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
                    None => (self.run_tool(chat, turn_id, call, events).await, true),
                };
                events.send(AgentEvent::ToolCallCompleted {
                    call_id: call.call_id,
                    output: output.clone(),
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
                        .store
                        .resolve_server_tool_call(call.call_id, &resolution, Utc::now())
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
        preceding_assistant: Option<&str>,
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
            if let Some(text) = preceding_assistant {
                self.persist(chat.id, turn_id, Role::Assistant, text)
                    .await?;
            }
        }
        for msg in msgs {
            self.persist(chat.id, turn_id, Role::User, &msg.content)
                .await?;
            transcript.push(ChatMessage {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: msg.content.clone(),
                }],
            });
            events.send(AgentEvent::UserSteered {
                content: msg.content,
            });
        }
        let preceding = preceding_assistant
            .filter(|text| !text.is_empty() && !durable.is_empty())
            .map(|text| Message {
                id: MessageId::new(),
                chat_id: chat.id,
                turn_id,
                role: Role::Assistant,
                content: text.to_owned(),
                created_at: Utc::now(),
            });
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
    ) -> ToolOutput {
        let Some(tool) = self.tools.get(&call.name) else {
            return ToolOutput::error(format!("unknown tool: {}", call.name));
        };
        // v1 policy: ReadOnly/Workspace auto; Sensitive parks on the approval gate.
        // Arm *before* emitting ApprovalRequired so a client that sees the event
        // can never race a 404 against a not-yet-parked call.
        if matches!(tool.approval_class(), ApprovalClass::Sensitive) {
            let summary = format!("{} requires approval", call.name);
            let pending = self.approvals.arm(ApprovalRequest {
                call_id: call.call_id,
                chat_id: chat.id,
                turn_id,
                tool_name: call.name.clone(),
                class: ApprovalClass::Sensitive,
                summary: summary.clone(),
            });
            events.send(AgentEvent::ApprovalRequired {
                call_id: call.call_id,
                class: ApprovalClass::Sensitive,
                summary,
            });
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
        let ctx = self.config.tool_scratch.as_ref().map_or_else(
            || ToolCtx::without_private_scratch(chat.id, chat.project_id),
            |scratch| ToolCtx::with_private_scratch(chat.id, chat.project_id, scratch.clone()),
        );
        let mut output = match tool.execute(&ctx, parse_args(&call.args)).await {
            Ok(output) => output,
            Err(err) => ToolOutput::error(err.to_string()),
        };
        if let Some(truncated) =
            truncate_to_bytes(&output.content, self.config.max_tool_result_bytes)
        {
            output.content = truncated;
        }
        output
    }

    async fn persist(
        &self,
        chat_id: crate::id::ChatId,
        turn_id: TurnId,
        role: Role,
        content: &str,
    ) -> Result<()> {
        self.store
            .append_message(&Message {
                id: MessageId::new(),
                chat_id,
                turn_id,
                role,
                content: content.to_string(),
                created_at: Utc::now(),
            })
            .await
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
            &self.tools.specs(),
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

/// Partition calls into per-model-step batches (see [`rebuild_transcript`]).
fn batch_tool_calls(tool_calls: &[ToolCallRecord]) -> Vec<Vec<&ToolCallRecord>> {
    let mut batches: Vec<Vec<&ToolCallRecord>> = Vec::new();
    let mut current: Vec<&ToolCallRecord> = Vec::new();
    let mut batch_done_at: Option<chrono::DateTime<Utc>> = None;

    for call in tool_calls {
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
    use std::sync::atomic::{AtomicUsize, Ordering};
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
    use crate::tools::ReadFile;

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
                ClaimedAgentEvent::Flush(_) => panic!("unhandled claimed-event flush"),
            })
            .collect()
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
        name: &'static str,
        arguments: &'static str,
    }

    struct ContextRecordingTool {
        observed_project: Arc<Mutex<Option<Option<ProjectId>>>>,
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
    async fn claimed_agent_retries_a_mixed_client_call_then_preserves_exhausted_usage() {
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
            attachment_revision: 0,
            root_attachments: Vec::new(),
            created_at: Utc::now(),
        };
        store.create_chat(&chat).await.unwrap();
        let observed_project = Arc::new(Mutex::new(None));
        let tools = Arc::new(ToolRegistry::new().with(Box::new(ContextRecordingTool {
            observed_project: observed_project.clone(),
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
        fn arm(&self, _request: ApprovalRequest) -> crate::approval::ApprovalFuture<'_> {
            if let Some(tx) = self.armed.lock().unwrap().take() {
                let _ = tx.send(());
            }
            Box::pin(future::pending())
        }
    }

    /// Trips cancel, then resolves Approve immediately — both arms of the
    /// approval `select` are ready in the same poll. Without a cancel-preferring
    /// check, `select` would take Approve and the Sensitive tool would run.
    struct CancelThenApproveGate {
        cancel: CancelToken,
    }

    impl ApprovalGate for CancelThenApproveGate {
        fn arm(&self, _request: ApprovalRequest) -> crate::approval::ApprovalFuture<'_> {
            self.cancel.cancel();
            Box::pin(async { ApprovalDecision::Approve })
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
            attachment_revision: 0,
            root_attachments: Vec::new(),
            created_at: Utc::now(),
        };
        store.create_chat(&chat).await.unwrap();
        (store, chat, workspace)
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
                AgentEvent::UserSteered { content } => {
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
