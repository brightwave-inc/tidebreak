use std::collections::{HashMap, HashSet};
use std::sync::atomic::AtomicI32;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use futures::channel::mpsc::UnboundedSender;
use futures::future::{self, Either};
use futures::StreamExt;
use futures_timer::Delay;

use crate::approval::{ApprovalGate, RefuseGate, StandingGrants};
use crate::cancel::CancelToken;
use crate::citation::{parse_assistant_citations, AssistantCitationInput};
use crate::context;
use crate::error::{AgentError, Result};
use crate::event::AgentEvent;
use crate::id::{CallId, ChatId, MessageId, TurnId};
use crate::model::{
    Chat, Message, Role, ToolCallExecution, ToolCallRecord, ToolCallResolution, ToolCallStatus,
    TurnRunStatus,
};
use crate::preview::ToolResultPreview;
use crate::provider::{
    ChatMessage, ChatRequest, ContentBlock, MessageReasoning, ModelProvider, ProviderEvent,
    ReasoningOrigin, RefusalDetails, RefusalOutcome, StopReason, Usage,
};
use crate::steer::SteerInbox;
use crate::storage::{
    AcceptClaimedToolCallOutcome, AcceptToolCallOutcome, AppendClaimedMessageOutcome,
    ApplyTurnSteerOutcome, BlobStore, JournaledTurnSteerOutcome, ResolveToolCallOutcome, Store,
    TurnLeaseFence,
};
use crate::tool::{ToolErrorCategory, ToolOutput};
use crate::PermissionMode;

use super::events::{AgentProgress, AssistantStreamEventFilter, ClaimedAgentEvent, EventSink};
use super::registry::ToolRegistry;
use super::transcript::{parse_args, parse_tool_args, tool_result_blocks};
use super::types::{AgentConfig, AgentTurnOutcome, SandboxAgentSpawnRequest, WRAP_UP_INSTRUCTION};
use super::{
    AcceptedServerCall, Agent, AssistantCandidate, CallIsolation, ClientArgumentResolution,
    PendingCall, SandboxSpawnGate, StreamAttempt, StreamEnd, StreamItem, TurnExecution,
    MAX_PARALLEL_READ_ONLY_CALLS, REPEATED_CALL_LIMIT,
};

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
            blobs: None,
            config,
            approvals: Arc::new(RefuseGate),
            standing_grants: Arc::new(StandingGrants::new()),
            cancel: CancelToken::new(),
            steer: SteerInbox::new(),
            durable_steer_lease: None,
            agent_orchestration_enabled: false,
            continuation_instruction: None,
            pending_sandbox_spawns: Vec::new(),
            pending_sandbox_spawn_steer_revision: None,
        }
    }

    /// Hydrate image attachments for outbound requests from `blobs`.
    ///
    /// Without a byte source an agent cannot honour a transcript that carries
    /// image blocks, so it evicts them to text stand-ins rather than handing an
    /// adapter a block it must refuse. Wire this wherever the store can return
    /// messages with attachments.
    #[must_use]
    pub fn with_blobs(mut self, blobs: Arc<dyn BlobStore>) -> Self {
        self.blobs = Some(blobs);
        self
    }

    /// Use `gate` for Sensitive-tool decisions (park-and-resume on the server).
    #[must_use]
    pub fn with_approvals(mut self, gate: Arc<dyn ApprovalGate>) -> Self {
        self.approvals = gate;
        self
    }

    /// Provide explicit non-durable grants for an embedded caller. The
    /// foreground server intentionally does not call this: user-approved
    /// grants are persisted and matched by its approval broker transaction.
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

    /// Continue checkpointing sandbox siblings from a model step already
    /// evaluated by an earlier segment of this turn.
    #[must_use]
    pub fn with_pending_sandbox_spawns(
        mut self,
        pending: Vec<SandboxAgentSpawnRequest>,
        steer_revision: Option<i64>,
    ) -> Self {
        self.pending_sandbox_spawns = pending;
        self.pending_sandbox_spawn_steer_revision = steer_revision;
        self
    }

    pub(crate) fn agent_orchestration_active(&self) -> bool {
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

    pub(crate) async fn run_turn_inner(
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
                        retry_after: err.retry_after(),
                        usage: progress.usage,
                        model_steps: progress.model_steps,
                    });
                }
                Err(err)
            }
        }
    }

    pub(crate) async fn drive(
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
        if !self.pending_sandbox_spawns.is_empty() {
            let Some(mut steer_revision) = self.pending_sandbox_spawn_steer_revision else {
                return Err(AgentError::Store(
                    "pending sandbox spawns require a durably claimed turn".into(),
                ));
            };
            // A steer can land after one sibling's approval commits but before
            // its spawn checkpoint. Apply it before replaying that admitted
            // request, then fence the checkpoint with the resulting durable
            // revision. If the whole carried queue is refused below, the
            // transcript reload that follows picks these persisted steers up.
            let mut applied_steers = Vec::new();
            self.apply_steers(chat, turn_id, &mut applied_steers, None, events)
                .await?;
            if let Some(revision) = self.durable_generation_revision(turn_id).await? {
                steer_revision = revision;
            }
            // An already-gated head keeps the approval the reader committed;
            // every still-ungated sibling passes the gate independently. A
            // card for one delegation is not consent for the rest, and a
            // refused sibling is answered in place before the batch moves on.
            for index in 0..self.pending_sandbox_spawns.len() {
                let request = self.pending_sandbox_spawns[index].clone();
                let gate = if request.approval_gated {
                    SandboxSpawnGate::Admit(request)
                } else {
                    self.gate_sandbox_spawn(chat, turn_id, &request, events)
                        .await?
                };
                match gate {
                    SandboxSpawnGate::Admit(request) => {
                        return Ok(AgentTurnOutcome::SandboxAgentSpawn {
                            request,
                            remaining_requests: self.pending_sandbox_spawns[index + 1..].to_vec(),
                            usage: Usage::default(),
                            steer_revision,
                            model_steps: 0,
                        });
                    }
                    // Answered durably and published; the transcript rebuild
                    // below picks the refusal up as this call's result.
                    SandboxSpawnGate::Declined(_) => {}
                }
            }
        }
        // The provider transcript for this turn: prior stored text + the blocks
        // we build up as the loop runs.
        // A checkpoint is optional cache data. A stale, malformed, or
        // temporarily unreadable record must never block the user turn: the
        // deterministic transcript reduction below remains the safe fallback.
        let mut checkpoint = self.load_projectable_checkpoint(chat.id).await;
        let loaded = self
            .load_transcript(
                chat.id,
                checkpoint
                    .as_ref()
                    .map(|checkpoint| checkpoint.source_message_id),
            )
            .await?;
        let mut checkpoint_boundary = loaded.checkpoint_boundary;
        let source_boundaries = loaded.source_boundaries;
        let user_texts = loaded.user_texts;
        let mut transcript = loaded.messages;
        if let Some(instruction) = self.continuation_instruction.as_ref() {
            transcript.push(ChatMessage::text(Role::System, instruction.clone()));
        }
        let mut total_usage = Usage::default();
        // Provider adapters enforce vendor-search limits per request, while
        // Tidebreak's contract is per turn. Offer the allowance to at most one
        // foreground request: once an egress attempt receives the full cap,
        // Tidebreak cannot prove how much it spent after a failed or partial
        // stream, so every retry and later model step must fail closed to none.
        let mut remaining_vendor_web_search = match self.config.web_search {
            super::types::TurnWebSearch::Vendor(vendor) if vendor.max_uses > 0 => Some(vendor),
            _ => None,
        };
        self.resume_pending_server_calls(chat, turn_id, events, &mut transcript)
            .await?;
        let mut reduction_level: u32 = 0;
        let mut checkpoint_attempt_boundary = None;
        // The current run of consecutive identical plain server calls — the
        // (name, canonical arguments) pair and how many of it have executed.
        // Deliberately in-memory and per-attempt: the streak is a nudge to a
        // live model, not part of the durable record, so it never survives a
        // turn boundary or a crash-recovery resume.
        let mut repeat_streak: Option<((String, String), usize)> = None;

        // One iteration past the budget is the wrap-up: the model is told the
        // turn is over and asked for a closing answer it may not use tools for,
        // so exhausting the budget ends in a real message rather than an error.
        // A zero budget is the degenerate case of the same contract: a lease
        // segment resuming after the budget was spent — a parked checkpoint on
        // the last budgeted step, or a retried wrap-up failure — goes straight
        // to the wrap-up call, which is safe to admit because it consumes no
        // budget and cannot ask for another round (#1181).
        //
        // The iteration after *that* exists only for a provider that accepts
        // `tool_choice: none` and calls a tool regardless; it runs only when
        // the flag below is set, and only ever once.
        let mut wrap_up_without_tools = false;
        for step in 0..=self.config.max_steps + 1 {
            let wrap_up = step >= self.config.max_steps;
            let wrap_up_retry = step > self.config.max_steps;
            if wrap_up_retry && !wrap_up_without_tools {
                break;
            }
            // The wrap-up call is outside the budget. Counting it would let a
            // resumed attempt inherit a step debt, and would make the turn's
            // reported step count exceed the ceiling it just respected.
            let steps_before = step.min(self.config.max_steps);
            let steps_used = (step + 1).min(self.config.max_steps);
            // Between steps: stop before starting a fresh model call if cancelled.
            if self.cancel.is_cancelled() {
                return Ok(self.finish_cancelled(
                    events,
                    total_usage,
                    steps_before,
                    publish_terminal,
                    None,
                ));
            }
            if wrap_up && !wrap_up_retry {
                transcript.push(ChatMessage::text(Role::System, WRAP_UP_INSTRUCTION));
            }
            // Boundary steer: inject any queued messages before the next model call.
            self.apply_steers(chat, turn_id, &mut transcript, None, events)
                .await?;
            // Fence this exact provider request, not the later worker handoff.
            // A steer applied after this snapshot must supersede its output;
            // one applied at the boundary above is already part of the prompt.
            let generation_steer_revision = self.durable_generation_revision(turn_id).await?;

            // Fit the transcript to the context window, retrying this same
            // step with tighter budgets on prompt-too-long errors. A provider
            // may report the overflow before returning a stream or after
            // streaming a partial candidate; both rejoin this attempt loop.
            let StreamAttempt {
                end: stream_end,
                text,
                mut calls,
                items,
                mut reasoning,
                stop_reason,
                refusal_details,
            } = 'step_attempt: loop {
                let stream = loop {
                    let mut prefix = self
                        .build_request_prefix(
                            chat,
                            &transcript,
                            reduction_level,
                            checkpoint.as_ref(),
                            checkpoint_boundary,
                        )
                        .await?;
                    // Compaction rides this exact prefix, so it is assembled
                    // first and the call only appends to it. The wrap-up step is
                    // skipped: it constrains `tool_choice`, which costs the
                    // message cache a ride-along would have read, and a
                    // checkpoint written there cannot shorten a turn that is
                    // already writing its last answer.
                    let created = if wrap_up {
                        None
                    } else {
                        self.maybe_create_context_checkpoint(
                            super::context::CreateContextCheckpoint {
                                chat_id: chat.id,
                                transcript: &transcript,
                                source_boundaries: &source_boundaries,
                                user_texts: &user_texts,
                                current: checkpoint.as_ref(),
                                attempted_boundary: &mut checkpoint_attempt_boundary,
                                events,
                                prefix: &prefix,
                                // The threshold is the automatic trigger, and
                                // this pass answers no particular request.
                                ignore_threshold: false,
                                focus: None,
                            },
                        )
                        .await?
                    };
                    if let Some(created) = created {
                        checkpoint_boundary = source_boundaries
                            .iter()
                            .find(|source| source.message_id == created.source_message_id)
                            .map(|source| source.provider_boundary);
                        checkpoint = Some(created);
                        // The new checkpoint stands in for the prefix the step
                        // was about to send, so the request is reassembled
                        // around it. One rebuild is enough: the boundary just
                        // advanced and the attempt fence is set, so a second
                        // pass could not compact again.
                        prefix = self
                            .build_request_prefix(
                                chat,
                                &transcript,
                                reduction_level,
                                checkpoint.as_ref(),
                                checkpoint_boundary,
                            )
                            .await?;
                    }
                    // Cancellation may have arrived while the maintenance stream
                    // was active. Its usage belongs to the checkpoint record, not
                    // the foreground turn, and no user model call should begin.
                    if self.cancel.is_cancelled() {
                        return Ok(self.finish_cancelled(
                            events,
                            total_usage,
                            steps_before,
                            publish_terminal,
                            None,
                        ));
                    }
                    let reduced = prefix.reduced;
                    let fitted_tokens = context::estimate_transcript_tokens(&prefix.messages);
                    // `tool_choice` is only meaningful alongside a tool array.
                    // A chat-only config (or an empty tool surface) that sent
                    // `none` with no tools is a pairing providers reject — and
                    // it would hard-fail the one step that exists to guarantee
                    // an answer. With no tools the step is already terminal by
                    // construction, so the control has nothing to add.
                    let advertises_tools = !prefix.tools.is_empty();
                    let vendor_web_search = if wrap_up {
                        None
                    } else {
                        remaining_vendor_web_search.take()
                    };
                    let request = ChatRequest {
                        provider: self.config.provider.clone(),
                        conversation: Some(chat.id),
                        model: self.config.model.clone(),
                        reasoning_model: self.config.reasoning_model,
                        system: self.config.system_prompt.clone(),
                        messages: prefix.messages,
                        // Forbidding a tool call is what makes the wrap-up
                        // terminal. Withholding the schemas would do it too, but
                        // tools render at the front of the request, so an empty
                        // array shares no cached prefix with anything this chat
                        // has sent — full price on the largest transcript of the
                        // turn, to save nothing. The empty array is held back
                        // for the retry a provider that ignored `none` forces,
                        // where structural termination is the only guarantee
                        // left and the cache is already forfeit.
                        tools: if wrap_up_retry {
                            Vec::new()
                        } else {
                            prefix.tools
                        },
                        tool_choice: (wrap_up && !wrap_up_retry && advertises_tools)
                            .then_some(crate::provider::ToolChoice::None),
                        max_tokens: self.config.max_tokens,
                        temperature: self.config.temperature,
                        reasoning_effort: self.config.reasoning_effort,
                        vendor_web_search,
                        images: prefix.images,
                        ..Default::default()
                    };

                    progress.model_steps = steps_used;
                    match self.provider.stream(request).await {
                        Ok(stream) => {
                            // Tell clients the history was shortened for this call so
                            // a UI can surface it. Emitted only for the request that
                            // actually went out (after any retry climb).
                            if reduced {
                                // Against what the step would have sent, not
                                // the raw transcript: a prefix a checkpoint
                                // already stands in for was never headed out,
                                // so counting it would overstate the cut.
                                let original_tokens = self.model_view_tokens(
                                    &transcript,
                                    checkpoint.as_ref(),
                                    checkpoint_boundary,
                                );
                                events.send(AgentEvent::ContextTruncated {
                                    original_tokens: original_tokens as u32,
                                    fitted_tokens: fitted_tokens as u32,
                                });
                            }
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
                let attempt = self
                    .read_stream(stream, events, &mut total_usage, progress)
                    .await?;
                // A stream that broke mid-flight left this step's tool-call
                // arguments possibly truncated mid-JSON. Nothing here is safe to
                // act on, and nothing was persisted, so fail the turn under the
                // classified provider error rather than executing the fragment.
                if let StreamEnd::Failed(error) = &attempt.end {
                    events.send(AgentEvent::StreamInterrupted);
                    let error = error.clone().into_agent_error();
                    if matches!(error, AgentError::PromptTooLong(_))
                        && reduction_level < context::MAX_REDUCTION_LEVEL
                    {
                        reduction_level += 1;
                        continue 'step_attempt;
                    }
                    return Err(error);
                }
                // Prefer cancel when both cancel and interrupt are ready (cancel is
                // the left arm of the nested select). Also catch a cancel that raced
                // the final stream event.
                reduction_level = 0;
                break 'step_attempt attempt;
            };
            let has_provider_executed = items
                .iter()
                .any(|item| matches!(item, StreamItem::ProviderExecuted { .. }));
            if matches!(stream_end, StreamEnd::Cancelled) || self.cancel.is_cancelled() {
                // Calls that started before the cancel were already journaled,
                // so terminalizing silently would leave replay and live clients
                // holding a call that never resolves. Mark them discarded the
                // way the refusal path does — but only when calls had actually
                // started, because the marker also clears streamed prose and a
                // cancel with prose alone deliberately retains it.
                if !calls.is_empty() {
                    events.send(AgentEvent::StreamInterrupted);
                }
                // The prose the reader was watching survives the stop: a
                // text-only step hands its partial output to the terminal
                // commit so reload and the next model turn keep what the user
                // already saw (#1182). A step whose calls started was
                // discarded whole just above and stays message-less.
                let partial = if calls.is_empty() {
                    let parsed = parse_assistant_citations(&text);
                    if parsed.content.trim().is_empty() {
                        None
                    } else {
                        let message_id = if publish_terminal {
                            MessageId::new()
                        } else {
                            output_message_id
                        };
                        let candidate = AssistantCandidate {
                            message_id,
                            content: parsed.content,
                            citations: parsed.citations,
                            // A cancel can cut the stream between reasoning
                            // blocks, and a provider that validates replay
                            // checks the prefix against what it generated. A
                            // partial set is not that, so this message keeps
                            // none.
                            reasoning: MessageReasoning::default(),
                        };
                        let message = candidate.message(message_id, chat.id, turn_id);
                        if publish_terminal {
                            self.append_assistant_exact_retry(&message, &candidate.citations)
                                .await?;
                        }
                        Some((message, candidate.citations))
                    }
                } else {
                    None
                };
                return Ok(self.finish_cancelled(
                    events,
                    total_usage,
                    steps_used,
                    publish_terminal,
                    partial,
                ));
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

            let refused = stop_reason == StopReason::Refusal;
            if refused {
                // A refusal terminalizes the candidate. Tool arguments emitted
                // before it are incomplete and must never execute.
                if !calls.is_empty() {
                    Self::publish_deferred_host_stream_events(&items, &calls, events);
                    // Those calls were already journaled as they streamed, so
                    // clearing them silently would leave replay and live
                    // clients holding calls that never resolve. Mark them
                    // discarded the way the steer and stream-failure paths do.
                    events.send(AgentEvent::StreamInterrupted);
                }
                calls.clear();
            }

            // The wrap-up call forbade tool calls, so a call here is a provider
            // anomaly, not a decision to act. Answer each one so no client is
            // left holding a call that never resolves, then drop them: there is
            // no step left to run them in, and admitting them would ask the loop
            // for a round it has already refused. The prose survives — losing
            // text the reader can already see is the failure this whole path
            // exists to avoid, so a discard marker is deliberately not sent.
            if wrap_up && !calls.is_empty() {
                Self::publish_deferred_host_stream_events(&items, &calls, events);
                for call in &calls {
                    self.decline_call(
                        call,
                        events,
                        "not run: this turn reached its step limit, and this reply is its last. Say what you have.".into(),
                    );
                }
                calls.clear();
                // Declined calls and no prose is not an answer: the turn would
                // fail as empty, and the worker's retry would re-enter the same
                // wrap-up and burn another attempt on a provider that has
                // already shown it does not honour `tool_choice: none`. Ask
                // once more with no tools on the request at all, which is
                // terminal by construction rather than by the provider's
                // cooperation. Nothing was appended to the transcript for this
                // step, so the retry asks the same question.
                if !wrap_up_retry && text.trim().is_empty() {
                    // The abandoned leg may have streamed reasoning a live
                    // client already rendered; mark it discarded so the
                    // retry's answer starts a fresh bubble instead of
                    // inheriting thinking that led nowhere.
                    events.send(AgentEvent::StreamInterrupted);
                    wrap_up_without_tools = true;
                    continue;
                }
            }

            let candidate_message_id = if calls.is_empty() && !publish_terminal {
                output_message_id
            } else {
                MessageId::new()
            };
            let parsed = parse_assistant_citations(&text);
            let candidate = AssistantCandidate {
                message_id: candidate_message_id,
                content: parsed.content,
                citations: parsed.citations,
                reasoning: MessageReasoning::captured(
                    self.reasoning_origin(),
                    std::mem::take(&mut reasoning),
                ),
            };
            let text = &candidate.content;
            let refusal = refused.then(|| {
                RefusalOutcome::new(refusal_details.unwrap_or_default(), !text.is_empty())
            });

            // Foreground control calls are protocol boundaries, not ordinary
            // batch members. Reject a mixed step before any assistant message,
            // tool-call row, provider-native receipt, or sibling side effect is
            // persisted. The transient tool results give the model an explicit
            // correction while a retry after a crash safely starts from the
            // last durable transcript, where none of this invalid batch exists.
            if let Some(control_name) = calls
                .iter()
                .find_map(|call| self.standalone_control_name(call))
                .filter(|_| calls.len() != 1 || has_provider_executed || !text.trim().is_empty())
            {
                if !text.trim().is_empty() {
                    events.send(AgentEvent::StreamInterrupted);
                }
                let correction = format!(
                    "not run: `{control_name}` must be called alone, with no assistant text or sibling tool calls. No calls from this step were executed or persisted. Reissue only `{control_name}` in a fresh model step."
                );
                Self::publish_deferred_host_stream_events(&items, &calls, events);
                let outputs = calls
                    .iter()
                    .map(|call| self.decline_call(call, events, correction.clone()))
                    .collect::<Vec<_>>();
                let blocks = Self::stream_content_blocks(&items, &calls);
                // The rejected step is not what the provider generated: we
                // declined the control and will ask again. Anthropic validates
                // the latest assistant thinking prefix against the original
                // blocks, so a captured set here 400s the retry. Cancel
                // already drops partial reasoning for the same reason.
                transcript.push(ChatMessage {
                    role: Role::Assistant,
                    content: blocks,
                    reasoning: MessageReasoning::default(),
                });
                transcript.push(ChatMessage {
                    role: Role::User,
                    reasoning: MessageReasoning::default(),
                    content: calls
                        .iter()
                        .zip(outputs)
                        .flat_map(|(call, output)| {
                            tool_result_blocks(
                                call.provider_id.clone(),
                                self.tool_result_for_model(&output.content, call.call_id),
                                output.is_error,
                                &output.images,
                                self.config.image_input,
                            )
                        })
                        .collect(),
                });
                repeat_streak = None;
                continue;
            }

            // A valid model-declared blocker is terminal by construction: the
            // call is recorded as a successful control operation, its
            // explanation becomes the assistant message, and the existing
            // refused lifecycle supplies non-success semantics to every driver.
            if calls.len() == 1 && self.is_report_blocked_call(&calls[0]) {
                let call = &calls[0];
                let parsed = parse_tool_args(&call.args).and_then(|arguments| {
                    crate::agent_tools::parse_report_blocked_arguments(&arguments)
                });
                let Some(blocked) = parsed else {
                    let output = self.decline_call(
                        call,
                        events,
                        "not run: `report_blocked` requires exactly a lowercase `reason_code` and a concise non-empty `explanation`, with no extra properties. Correct the arguments and reissue `report_blocked` alone."
                            .into(),
                    );
                    transcript.push(ChatMessage {
                        role: Role::Assistant,
                        content: vec![ContentBlock::ToolUse {
                            id: call.provider_id.clone(),
                            name: call.name.clone(),
                            input: parse_args(&call.args).0,
                        }],
                        reasoning: candidate.reasoning.clone(),
                    });
                    transcript.push(ChatMessage {
                        role: Role::User,
                        reasoning: MessageReasoning::default(),
                        content: tool_result_blocks(
                            call.provider_id.clone(),
                            self.tool_result_for_model(&output.content, call.call_id),
                            true,
                            &output.images,
                            self.config.image_input,
                        ),
                    });
                    repeat_streak = None;
                    continue;
                };
                let Some(steer_revision) = generation_steer_revision else {
                    let output = self.decline_call(
                        call,
                        events,
                        "not run: `report_blocked` is available only from a durably claimed foreground turn. Continue with the capabilities available on this surface."
                            .into(),
                    );
                    transcript.push(ChatMessage {
                        role: Role::Assistant,
                        content: vec![ContentBlock::ToolUse {
                            id: call.provider_id.clone(),
                            name: call.name.clone(),
                            input: parse_args(&call.args).0,
                        }],
                        reasoning: candidate.reasoning.clone(),
                    });
                    transcript.push(ChatMessage {
                        role: Role::User,
                        reasoning: MessageReasoning::default(),
                        content: tool_result_blocks(
                            call.provider_id.clone(),
                            self.tool_result_for_model(&output.content, call.call_id),
                            true,
                            &output.images,
                            self.config.image_input,
                        ),
                    });
                    repeat_streak = None;
                    continue;
                };
                self.ensure_durable_lease_current(turn_id).await?;
                self.persist_report_blocked_call(
                    chat.id,
                    turn_id,
                    call,
                    &blocked.explanation,
                    events,
                )
                .await?;
                let output = AssistantCandidate {
                    message_id: output_message_id,
                    content: blocked.explanation,
                    citations: Vec::new(),
                    reasoning: candidate.reasoning,
                }
                .message(output_message_id, chat.id, turn_id);
                return Ok(AgentTurnOutcome::Completed {
                    output,
                    citations: Vec::new(),
                    usage: total_usage,
                    stop_reason: StopReason::Refusal,
                    refusal: Some(RefusalOutcome::new(
                        RefusalDetails::from_category(Some("blocked")),
                        true,
                    )),
                    steer_revision: Some(steer_revision),
                    model_steps: steps_used,
                });
            }

            // Sequence the batch rather than refuse its shape. A refusal
            // discarded the whole step — the assistant's prose and every
            // sibling call that had already succeeded — to ask for a form the
            // model cannot reliably produce, because providers parallelise
            // tool calls by design. The order below is the fix: plain server
            // calls run first, approval-bearing calls follow one at a time,
            // and the one call that has to stand alone is taken last, once
            // everything else is terminal.
            let isolations: Vec<Option<CallIsolation>> =
                calls.iter().map(|call| self.call_isolation(call)).collect();
            let sensitives: Vec<bool> = calls
                .iter()
                .enumerate()
                .map(|(index, call)| isolations[index].is_none() && self.call_is_sensitive(call))
                .collect();
            let isolated = isolations.iter().position(Option::is_some);

            // A model stuck re-issuing one call verbatim learns nothing from
            // the repeats: identical arguments already produced their answer.
            // Count consecutive identical plain calls and, once the streak
            // reaches the limit, answer the next one without running it.
            // Decided here, before admission, so a refused call still flows
            // through the ordinary resolution path and its durable row
            // terminalizes like any other failure. Approval-bearing and
            // isolated calls are exempt — their gates carry their own
            // guidance — and they break the streak like any other change of
            // course.
            let mut repeat_refusals: Vec<Option<String>> = Vec::with_capacity(calls.len());
            for (index, call) in calls.iter().enumerate() {
                if isolations[index].is_some() || sensitives[index] {
                    repeat_streak = None;
                    repeat_refusals.push(None);
                    continue;
                }
                let key = (call.name.clone(), parse_args(&call.args).0.to_string());
                let refusal = match repeat_streak.as_mut() {
                    Some((streak_key, count)) if *streak_key == key => {
                        if *count >= REPEATED_CALL_LIMIT {
                            Some(format!(
                                "not run: this exact call has now been made {REPEATED_CALL_LIMIT} times in a row with the same arguments. Change the arguments or the approach, or tell the user what you are stuck on.",
                            ))
                        } else {
                            *count += 1;
                            None
                        }
                    }
                    _ => {
                        repeat_streak = Some((key, 1));
                        None
                    }
                };
                repeat_refusals.push(refusal);
            }

            // This step is about to persist tool-call rows, execute server tool
            // side effects, and record the assistant message. Fence those on the
            // lease first: the provider stream just consumed may have outlasted
            // it, and a stale segment must neither commit nor replay an effect a
            // later attempt now owns. Terminal completion is left to the worker's
            // own lease compare-and-swap, so only fence the tool-bearing path.
            if !calls.is_empty() || has_provider_executed {
                self.ensure_durable_lease_current(turn_id).await?;
            }

            // Record the assistant message (text + any tool-use blocks).
            if !calls.is_empty() && (!text.is_empty() || !candidate.reasoning.is_empty()) {
                // A checkpoint returns from the loop and the resumed attempt
                // rebuilds its transcript from the store, so an unpersisted
                // prose preamble or provider-native tool replay block would be
                // lost. A tool-only Gemini step therefore writes an empty
                // assistant message solely as the durable replay carrier.
                self.persist_assistant(chat.id, turn_id, &candidate).await?;
            }

            // Reserve durable history order for every ordinary call and
            // provider-executed receipt before resolving the latter. Provider
            // receipts arrive complete, but terminalizing one before a later
            // same-stream host call is admitted would make transcript rebuild
            // treat that host call as a new model step.
            let mut recovered_results: HashMap<CallId, ToolOutput> = HashMap::new();
            let mut admitted_provider_calls = HashSet::new();
            for item in &items {
                match item {
                    StreamItem::HostCall(index) => {
                        let Some(call) = calls.get(*index) else {
                            continue;
                        };
                        if isolations[*index].is_none() && !sensitives[*index] {
                            if let Some(recovered) =
                                self.accept_server_call(chat.id, turn_id, call).await?
                            {
                                recovered_results.insert(call.call_id, recovered);
                            }
                        }
                    }
                    StreamItem::ProviderExecuted { call_id, block } => {
                        if self
                            .accept_provider_executed_call(chat.id, turn_id, *call_id, block)
                            .await?
                        {
                            admitted_provider_calls.insert(*call_id);
                        }
                    }
                    StreamItem::HostCallArgsDelta { .. } => {}
                }
            }

            // Reproduce the provider's arrival order in both the live activity
            // stream and the assistant transcript. Host events before the first
            // provider receipt were already forwarded while streaming; later
            // ones were held so the receipt can be announced in between.
            let mut blocks: Vec<ContentBlock> = Vec::new();
            if !text.is_empty() {
                blocks.push(ContentBlock::Text { text: text.clone() });
            }
            let mut provider_seen = false;
            for item in &items {
                match item {
                    StreamItem::HostCall(index) => {
                        let Some(call) = calls.get(*index) else {
                            continue;
                        };
                        if provider_seen {
                            events.send(AgentEvent::ToolCallStarted {
                                call_id: call.call_id,
                                name: call.name.clone(),
                            });
                        }
                        // The transcript block stays the coerced value: it goes
                        // back to the provider, whose tool-use input must be
                        // valid JSON. The garbled fragment is kept durably.
                        blocks.push(ContentBlock::ToolUse {
                            id: call.provider_id.clone(),
                            name: call.name.clone(),
                            input: parse_args(&call.args).0,
                        });
                    }
                    StreamItem::HostCallArgsDelta {
                        call_index,
                        fragment,
                    } => {
                        if provider_seen {
                            if let Some(call) = calls.get(*call_index) {
                                events.send(AgentEvent::ToolCallArgsDelta {
                                    call_id: call.call_id,
                                    fragment: fragment.clone(),
                                });
                            }
                        }
                    }
                    StreamItem::ProviderExecuted { call_id, block, .. } => {
                        if admitted_provider_calls.contains(call_id) {
                            self.complete_provider_executed_call(
                                chat.id, turn_id, *call_id, block, events,
                            )
                            .await?;
                        }
                        blocks.push(block.clone());
                        provider_seen = true;
                    }
                }
            }
            if !blocks.is_empty() {
                transcript.push(ChatMessage {
                    role: Role::Assistant,
                    content: blocks,
                    // The step's reasoning rides its assistant message for the
                    // rest of the turn, and rides the durable message the
                    // candidate writes so a later turn can replay it too. A
                    // steer, cancel, or broken stream discarded `reasoning`
                    // along with the step before here, so nothing partial ever
                    // reaches the transcript.
                    reasoning: candidate.reasoning.clone(),
                });
            }

            if calls.is_empty() {
                // A plain text step is a change of course, so it breaks any
                // repeated-call streak the previous steps had built up.
                repeat_streak = None;
                // Legacy turns persist each candidate immediately, so each needs
                // its own identity. A claimed turn keeps the caller's stable
                // completion identity: steered candidates are persisted
                // separately by `apply_steers`, and only the actual final output
                // uses it.
                let output = candidate.message(candidate.message_id, chat.id, turn_id);
                if publish_terminal && !text.is_empty() {
                    self.append_assistant_exact_retry(&output, &candidate.citations)
                        .await?;
                }
                // The in-process driver mirrors the worker's emptiness
                // detection (#1208): a final response with neither text nor a
                // tool call is not an answer, and completing on it reports a
                // successful turn that produced nothing. The disposition stays
                // where the worker owns it — there is no attempt budget here,
                // so instead of rescheduling the turn simply fails. Refusals
                // are exempt for the same reason the worker gives: the refusal
                // is the outcome and stays meaningful with no prose behind it.
                let empty_final = publish_terminal && refusal.is_none() && text.trim().is_empty();
                // Drain steers until the inbox is quiet, then complete. A steer
                // that arrives as the stream finished must continue the turn
                // rather than race a TurnCompleted. `try_complete` holds the
                // queue lock across the empty-check and terminal emit so a
                // concurrent push cannot 202 and then be orphaned — the
                // emptiness failure rides the same fence, so a steer that
                // arrives before sealing still continues the turn.
                loop {
                    if self.cancel.is_cancelled() {
                        // The answer is fully formed here; a cancel that races
                        // the completion fence keeps it rather than discarding
                        // a finished reply the user watched stream (#1182).
                        return Ok(self.finish_cancelled(
                            events,
                            total_usage,
                            steps_used,
                            publish_terminal,
                            (!output.content.trim().is_empty())
                                .then(|| (output.clone(), candidate.citations.clone())),
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
                            refusal: refusal.clone(),
                            steer_revision: generation_steer_revision,
                            model_steps: steps_used,
                        });
                    }
                    if self.steer.try_complete(|| {
                        if publish_terminal {
                            if let Some(refusal) = refusal.clone() {
                                events.send(AgentEvent::TurnRefused {
                                    usage: total_usage,
                                    refusal,
                                });
                            } else if !empty_final {
                                events.send(AgentEvent::TurnCompleted {
                                    usage: total_usage,
                                    stop_reason,
                                });
                            }
                            // An empty final response emits nothing here; the
                            // error below surfaces as TurnFailed.
                        }
                    }) {
                        if empty_final {
                            return Err(AgentError::msg(
                                "the model returned neither text nor a tool call",
                            ));
                        }
                        return Ok(AgentTurnOutcome::Completed {
                            output,
                            citations: candidate.citations.clone(),
                            usage: total_usage,
                            stop_reason,
                            refusal: refusal.clone(),
                            steer_revision: generation_steer_revision,
                            model_steps: steps_used,
                        });
                    }
                    // Steer arrived between drain and try_complete — loop.
                }
                continue;
            }

            // Tool calls made on the last budgeted step still run: the wrap-up
            // call that follows reads their results, so the work is not wasted
            // and the closing answer is written with it in hand.

            // Run the tool calls and feed the results back for the next step.
            // Outputs are collected by position so the results message keeps the
            // order the model asked in, whatever order they were produced.
            let mut outputs: Vec<Option<ToolOutput>> = vec![None; calls.len()];

            // Only a leading run of read-only calls can overlap. A workspace
            // mutation remains a sequencing boundary, so a later read cannot
            // race it and observe either side nondeterministically. The
            // isolated call is also a boundary: it is deliberately taken only
            // after every ordinary sibling is terminal.
            let parallel_prefix_len = calls
                .iter()
                .enumerate()
                .take_while(|(index, call)| {
                    isolations[*index].is_none() && self.call_is_parallel_eligible(call)
                })
                .count();
            // One call gains nothing from the concurrent path, and must still
            // flow through the ordinary sequential loop below.
            let parallel_batch_len = (parallel_prefix_len > 1).then_some(parallel_prefix_len);
            if let Some(parallel_batch_len) = parallel_batch_len {
                let parallel_results =
                    futures::stream::iter((0..parallel_batch_len).map(|index| {
                        let call = &calls[index];
                        let recovered = recovered_results.remove(&call.call_id);
                        let repeat_refusal = repeat_refusals[index].take();
                        async move {
                            (
                                index,
                                self.execute_server_call(
                                    chat,
                                    turn_id,
                                    call,
                                    events,
                                    recovered,
                                    repeat_refusal,
                                )
                                .await,
                            )
                        }
                    }))
                    .buffer_unordered(MAX_PARALLEL_READ_ONLY_CALLS)
                    .collect::<Vec<_>>()
                    .await;

                // Drain the whole batch before propagating a storage failure.
                // Every admitted sibling is then terminal rather than being
                // left pending behind an early error return.
                for (index, output) in parallel_results {
                    outputs[index] = Some(output?);
                }
                if self.cancel.is_cancelled() {
                    return Ok(self.finish_cancelled(
                        events,
                        total_usage,
                        steps_used,
                        publish_terminal,
                        None,
                    ));
                }
            }

            for (index, call) in calls.iter().enumerate() {
                if parallel_batch_len.is_some_and(|len| index < len)
                    || isolations[index].is_some()
                    || sensitives[index]
                {
                    continue;
                }
                outputs[index] = Some(
                    self.execute_server_call(
                        chat,
                        turn_id,
                        call,
                        events,
                        recovered_results.remove(&call.call_id),
                        repeat_refusals[index].take(),
                    )
                    .await?,
                );
                // A cancel that arrived during this tool (including while it was
                // parked on approval) stops the turn before the next model call.
                if self.cancel.is_cancelled() {
                    return Ok(self.finish_cancelled(
                        events,
                        total_usage,
                        steps_used,
                        publish_terminal,
                        None,
                    ));
                }
            }

            // Approval-bearing calls run after every plain sibling is terminal,
            // one at a time: a row is admitted only once the previous one has
            // resolved, so a call parked on the approval gate is always the
            // turn's only pending row and recovery never has to choose between
            // two. Nothing here is declined — a second Sensitive call simply
            // waits its turn.
            for (index, call) in calls.iter().enumerate() {
                if !sensitives[index] {
                    continue;
                }
                let recovered = self.accept_server_call(chat.id, turn_id, call).await?;
                outputs[index] = Some(
                    self.execute_server_call(chat, turn_id, call, events, recovered, None)
                        .await?,
                );
                if self.cancel.is_cancelled() {
                    return Ok(self.finish_cancelled(
                        events,
                        total_usage,
                        steps_used,
                        publish_terminal,
                        None,
                    ));
                }
            }

            // A reader's decline already tells the model what to do
            // differently, so it clears the repeat streak rather than stacking
            // a second layer of guidance on top: re-asking a declined call
            // goes back to the approval gate, not to the repetition guard.
            if outputs
                .iter()
                .flatten()
                .any(|output| output.error_category == Some(ToolErrorCategory::UserDeclined))
            {
                repeat_streak = None;
            }

            // A batch can name more than one call that has to stand alone. The
            // extras are answered rather than discarded: the model keeps its
            // prose and its finished work, and asks again next step. Nothing is
            // recorded for them because nothing ran, so a rebuilt transcript
            // carries neither the request nor the refusal.
            if let Some(taken) = isolated {
                for (index, call) in calls.iter().enumerate() {
                    if isolations[index].is_none()
                        || index == taken
                        || matches!(isolations[index], Some(CallIsolation::SandboxSpawn))
                    {
                        continue;
                    }
                    outputs[index] = Some(self.decline_call(
                        call,
                        events,
                        format!(
                            "not run: {} in the same step has to run on its own, so this step took only that call. Ask for this one again once it has finished.",
                            calls[taken].name
                        ),
                    ));
                }
            }

            // The isolated call, taken last so every sibling above is already
            // terminal: a checkpoint leaves nothing unfinished behind it for
            // the resuming attempt to guess about.
            if let Some(index) = isolated {
                let call = &calls[index];
                // Delegated agents execute with their own tool surface, so a
                // plan turn refuses to spawn or wait on them at all: the
                // read-only promise has to hold transitively, not just for
                // this agent's own calls.
                let plan_mode = matches!(chat.permission_mode, Some(PermissionMode::Plan));
                let plan_mode_blocks_orchestration = plan_mode
                    && matches!(
                        isolations[index],
                        Some(CallIsolation::SandboxSpawn | CallIsolation::AgentWait)
                    );
                let plan_mode_blocks_questions =
                    plan_mode && call.name == crate::ASK_USER_QUESTIONS_TOOL;
                if plan_mode_blocks_orchestration {
                    outputs[index] = Some(self.decline_call(
                        call,
                        events,
                        "not run: agent delegation is not available in plan mode; the chat is read-only until the reader leaves plan mode. Continue with read-only tools.".into(),
                    ));
                } else if plan_mode_blocks_questions {
                    outputs[index] = Some(self.decline_call(
                        call,
                        events,
                        "not run: ask_user_questions is not available in plan mode. Record missing inputs as assumptions or first steps in the plan and submit it with exit_plan_mode.".into(),
                    ));
                } else {
                    match isolations[index].expect("an isolated call has a class") {
                        CallIsolation::Client => {
                            match self.resolve_client_call_arguments(chat, call).await? {
                                ClientArgumentResolution::Refused(reason) => {
                                    outputs[index] = Some(self.decline_call(call, events, reason));
                                }
                                resolution => {
                                    let resolved = match resolution {
                                        ClientArgumentResolution::Resolved(arguments) => {
                                            Some(arguments)
                                        }
                                        _ => None,
                                    };
                                    match self.client_checkpoint(
                                        chat,
                                        turn_id,
                                        call,
                                        resolved,
                                        generation_steer_revision,
                                    ) {
                                        Ok((request, steer_revision)) => {
                                            return Ok(AgentTurnOutcome::ClientToolCall {
                                                request,
                                                usage: total_usage,
                                                steer_revision,
                                                model_steps: steps_used,
                                            })
                                        }
                                        Err(reason) => {
                                            outputs[index] = Some(self.decline_call(
                                                call,
                                                events,
                                                reason.into(),
                                            ));
                                        }
                                    }
                                }
                            }
                        }
                        CallIsolation::SandboxSpawn => {
                            match self.sandbox_checkpoint(call, generation_steer_revision) {
                                Ok((request, steer_revision)) => {
                                    // Every spawn the step named, in model
                                    // order. Each one passes the gate on its
                                    // own: a card about one delegation is not
                                    // consent for the others, and the first
                                    // that clears it leaves the loop carrying
                                    // the rest.
                                    let mut queued = vec![(index, request)];
                                    for (sibling, sibling_call) in
                                        calls.iter().enumerate().skip(index + 1)
                                    {
                                        if !matches!(
                                            isolations[sibling],
                                            Some(CallIsolation::SandboxSpawn)
                                        ) {
                                            continue;
                                        }
                                        match self
                                            .sandbox_checkpoint(sibling_call, Some(steer_revision))
                                        {
                                            Ok((request, _)) => queued.push((sibling, request)),
                                            Err(reason) => {
                                                outputs[sibling] = Some(self.decline_call(
                                                    sibling_call,
                                                    events,
                                                    reason.into(),
                                                ));
                                            }
                                        }
                                    }
                                    let mut admitted = None;
                                    for (position, (slot, request)) in queued.iter().enumerate() {
                                        match self
                                            .gate_sandbox_spawn(chat, turn_id, request, events)
                                            .await?
                                        {
                                            SandboxSpawnGate::Admit(request) => {
                                                admitted = Some((position, request));
                                                break;
                                            }
                                            // Refused before any child existed,
                                            // and already answered durably.
                                            SandboxSpawnGate::Declined(output) => {
                                                outputs[*slot] = Some(output);
                                            }
                                        }
                                    }
                                    if let Some((position, request)) = admitted {
                                        return Ok(AgentTurnOutcome::SandboxAgentSpawn {
                                            request,
                                            remaining_requests: queued[position + 1..]
                                                .iter()
                                                .map(|(_, request)| request.clone())
                                                .collect(),
                                            usage: total_usage,
                                            steer_revision,
                                            model_steps: steps_used,
                                        });
                                    }
                                }
                                Err(reason) => {
                                    outputs[index] =
                                        Some(self.decline_call(call, events, reason.into()));
                                }
                            }
                        }
                        CallIsolation::AgentWait => {
                            match self.agent_wait_checkpoint(call, generation_steer_revision) {
                                Ok((request, steer_revision)) => {
                                    return Ok(AgentTurnOutcome::WaitForAgents {
                                        request,
                                        usage: total_usage,
                                        steer_revision,
                                        model_steps: steps_used,
                                    })
                                }
                                Err(reason) => {
                                    outputs[index] =
                                        Some(self.decline_call(call, events, reason.into()));
                                }
                            }
                        }
                    }
                }
            }

            // Tool results ride in a user-role message (the Messages
            // convention). Every call the model made is answered here, so the
            // next step never sees a request it cannot account for.
            transcript.push(ChatMessage {
                role: Role::User,
                reasoning: MessageReasoning::default(),
                content: calls
                    .iter()
                    .zip(outputs)
                    .flat_map(|(call, output)| {
                        output.map_or_else(Vec::new, |output| {
                            tool_result_blocks(
                                call.provider_id.clone(),
                                self.tool_result_for_model(&output.content, call.call_id),
                                output.is_error,
                                &output.images,
                                self.config.image_input,
                            )
                        })
                    })
                    .collect(),
            });
            // Boundary steer after tools — injected before the next model step.
            self.apply_steers(chat, turn_id, &mut transcript, None, events)
                .await?;
        }

        // Only a wrap-up call that was itself abandoned — steered, or
        // interrupted and restarted — falls out of the loop. There is no step
        // left to write an answer in, so the turn ends as the failure it is.
        Ok(AgentTurnOutcome::Failed {
            error: crate::error::AgentErrorInfo {
                kind: "max_steps_exceeded".into(),
                message: "max steps per turn exceeded".into(),
            },
            retry_after: None,
            usage: total_usage,
            model_steps: self.config.max_steps,
        })
    }

    fn standalone_control_name<'a>(&self, call: &'a PendingCall) -> Option<&'a str> {
        let name = call.name.as_str();
        let registered = match name {
            crate::ASK_USER_QUESTIONS_TOOL | crate::agent_tools::REPORT_BLOCKED_TOOL => {
                self.durable_steer_lease.is_some() && self.tools.is_foreground_client(name)
            }
            crate::SPAWN_SANDBOX_AGENT_TOOL => {
                self.agent_orchestration_active() && self.tools.is_foreground_sandbox_spawn(name)
            }
            crate::WAIT_FOR_AGENTS_TOOL => {
                self.agent_orchestration_active() && self.tools.is_foreground_agent_wait(name)
            }
            _ => false,
        };
        registered.then_some(name)
    }

    fn is_report_blocked_call(&self, call: &PendingCall) -> bool {
        call.name == crate::agent_tools::REPORT_BLOCKED_TOOL
            && self.standalone_control_name(call).is_some()
    }

    async fn persist_report_blocked_call(
        &self,
        chat_id: ChatId,
        turn_id: TurnId,
        call: &PendingCall,
        explanation: &str,
        events: &EventSink<'_>,
    ) -> Result<()> {
        let (arguments, raw_arguments) = parse_args(&call.args);
        let record = ToolCallRecord {
            id: call.call_id,
            chat_id,
            turn_id,
            provider_id: call.provider_id.clone(),
            name: call.name.clone(),
            arguments,
            raw_arguments,
            execution: ToolCallExecution::Server,
            status: ToolCallStatus::Pending,
            result: None,
            result_preview: None,
            provider_replay: None,
            error_code: None,
            error_detail: None,
            client_executor_id: None,
            client_lease_expires_at: None,
            created_at: Utc::now(),
            resolved_at: None,
        };
        let needs_resolution = match self.accept_server_call_retry(&record).await? {
            AcceptedServerCall::Accepted => true,
            AcceptedServerCall::Existing(existing) if existing.status.is_terminal() => {
                if existing.status != ToolCallStatus::Completed
                    || existing.result.as_deref() != Some(explanation)
                {
                    return Err(AgentError::Store(format!(
                        "terminal report_blocked call {} does not match its explanation",
                        call.call_id
                    )));
                }
                false
            }
            AcceptedServerCall::Existing(_) => true,
            AcceptedServerCall::IdentityConflict => {
                return Err(AgentError::Store(format!(
                    "report_blocked call {} identity conflicts with its canonical request",
                    call.call_id
                )));
            }
            AcceptedServerCall::LeaseLost => {
                return Err(AgentError::Store(format!(
                    "turn {turn_id} lost its lease while accepting report_blocked call {}",
                    call.call_id
                )));
            }
        };
        let output = ToolOutput::text(explanation.to_owned());
        let preview = ToolResultPreview::build(&call.name, &output);
        if needs_resolution {
            self.resolve_server_call_retry(
                chat_id,
                turn_id,
                call.call_id,
                &ToolCallResolution::Completed {
                    result: explanation.to_owned(),
                },
                preview.as_ref(),
            )
            .await?;
        }
        events.send(AgentEvent::ToolCallCompleted {
            call_id: call.call_id,
            output: self.tool_output_for_event(&output, call.call_id),
            action: None,
            result: preview,
        });
        Ok(())
    }

    pub(crate) async fn read_stream(
        &self,
        mut stream: futures::stream::BoxStream<'static, ProviderEvent>,
        events: &EventSink<'_>,
        total_usage: &mut Usage,
        progress: &mut AgentProgress,
    ) -> Result<StreamAttempt> {
        let mut text = String::new();
        let mut calls = Vec::new();
        let mut items = Vec::new();
        let mut reasoning = Vec::new();
        let mut by_index = HashMap::new();
        let mut provider_seen = false;
        let mut stop_reason = StopReason::EndTurn;
        let mut refusal_details = None;
        let mut streamed_events = AssistantStreamEventFilter::new(events);
        let end = loop {
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
                ProviderEvent::ReasoningBlock { data } => reasoning.push(data),
                ProviderEvent::ToolCallStarted { index, id, name } => {
                    let call_id = CallId::new();
                    if !provider_seen {
                        streamed_events.send(AgentEvent::ToolCallStarted {
                            call_id,
                            name: name.clone(),
                        });
                    }
                    let call_index = calls.len();
                    by_index.insert(index, call_index);
                    items.push(StreamItem::HostCall(call_index));
                    calls.push(PendingCall {
                        call_id,
                        provider_id: id,
                        name,
                        args: String::new(),
                    });
                }
                ProviderEvent::ToolCallArgsDelta { index, fragment } => {
                    if let Some(&i) = by_index.get(&index) {
                        if !provider_seen {
                            streamed_events.send(AgentEvent::ToolCallArgsDelta {
                                call_id: calls[i].call_id,
                                fragment: fragment.clone(),
                            });
                        }
                        items.push(StreamItem::HostCallArgsDelta {
                            call_index: i,
                            fragment: fragment.clone(),
                        });
                        calls[i].args.push_str(&fragment);
                    }
                }
                ProviderEvent::Usage(reported) => {
                    // Usage accounts for provider work, not durable assistant
                    // output. A later StreamInterrupted may discard this
                    // candidate, but the reported tokens were still consumed.
                    *total_usage = total_usage.checked_add(reported).ok_or_else(|| {
                        AgentError::msg("provider usage exceeded the supported turn total")
                    })?;
                    progress.usage = *total_usage;
                }
                // The provider already ran this one and reported its result.
                // It joins the assistant message as a record of what happened;
                // it never becomes a `PendingCall`, so no dispatch path can
                // reach it.
                ProviderEvent::ProviderExecutedToolCall {
                    name,
                    input,
                    output,
                    is_error,
                    replay,
                } => {
                    let block = ContentBlock::ProviderExecutedToolCall {
                        name,
                        input,
                        output,
                        is_error,
                        replay,
                    };
                    if self.provider_executed_call_is_admissible(&block) {
                        items.push(StreamItem::ProviderExecuted {
                            call_id: CallId::new(),
                            block,
                        });
                        provider_seen = true;
                    }
                }
                ProviderEvent::Stop { reason } => stop_reason = reason,
                ProviderEvent::Refusal { details } => {
                    stop_reason = StopReason::Refusal;
                    refusal_details = Some(details);
                }
                ProviderEvent::Failed { error } => break StreamEnd::Failed(error),
            }
        };
        if matches!(end, StreamEnd::Steered | StreamEnd::Failed(_)) {
            streamed_events.discard();
        } else {
            streamed_events.finish();
        }
        if !matches!(end, StreamEnd::Done) {
            Self::publish_deferred_host_stream_events(&items, &calls, events);
        }
        Ok(StreamAttempt {
            end,
            text,
            calls,
            items,
            reasoning,
            stop_reason,
            refusal_details,
        })
    }

    fn publish_deferred_host_stream_events(
        items: &[StreamItem],
        calls: &[PendingCall],
        events: &EventSink<'_>,
    ) {
        let mut provider_seen = false;
        for item in items {
            match item {
                StreamItem::ProviderExecuted { .. } => provider_seen = true,
                StreamItem::HostCall(index) if provider_seen => {
                    if let Some(call) = calls.get(*index) {
                        events.send(AgentEvent::ToolCallStarted {
                            call_id: call.call_id,
                            name: call.name.clone(),
                        });
                    }
                }
                StreamItem::HostCallArgsDelta {
                    call_index,
                    fragment,
                } if provider_seen => {
                    if let Some(call) = calls.get(*call_index) {
                        events.send(AgentEvent::ToolCallArgsDelta {
                            call_id: call.call_id,
                            fragment: fragment.clone(),
                        });
                    }
                }
                StreamItem::HostCall(_) | StreamItem::HostCallArgsDelta { .. } => {}
            }
        }
    }

    fn stream_content_blocks(items: &[StreamItem], calls: &[PendingCall]) -> Vec<ContentBlock> {
        items
            .iter()
            .filter_map(|item| match item {
                StreamItem::HostCall(index) => {
                    calls.get(*index).map(|call| ContentBlock::ToolUse {
                        id: call.provider_id.clone(),
                        name: call.name.clone(),
                        input: parse_args(&call.args).0,
                    })
                }
                StreamItem::ProviderExecuted { block, .. } => Some(block.clone()),
                StreamItem::HostCallArgsDelta { .. } => None,
            })
            .collect()
    }

    /// Emit the cancellation terminal event and end the turn as a (non-error)
    /// success — the client asked for the stop, so it isn't a `TurnFailed`.
    ///
    /// `partial` carries prose the user was already reading so the worker can
    /// commit it durably with the cancellation; losing it made the next turn
    /// continue as though the answer was never given (#1182).
    pub(crate) fn finish_cancelled(
        &self,
        events: &EventSink<'_>,
        usage: Usage,
        model_steps: usize,
        publish_terminal_event: bool,
        partial: Option<(Message, Vec<AssistantCitationInput>)>,
    ) -> AgentTurnOutcome {
        if publish_terminal_event {
            events.send(AgentEvent::TurnCancelled { usage });
        }
        let (output, citations) = match partial {
            Some((message, citations)) => (Some(message), citations),
            None => (None, Vec::new()),
        };
        AgentTurnOutcome::Cancelled {
            output,
            citations,
            usage,
            model_steps,
        }
    }

    /// Drain the steer inbox into the transcript. Returns whether any messages
    /// were injected. Emits [`AgentEvent::UserSteered`] per message.
    pub(crate) async fn apply_steers(
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
                reasoning: MessageReasoning::default(),
            });
            events.send(AgentEvent::UserSteered {
                message_id,
                content: msg.content,
            });
        }
        let preceding = preceding_assistant
            .filter(|candidate| !candidate.content.is_empty() && !durable.is_empty())
            // A steered candidate is not the turn's output, so it takes an
            // identity of its own and its citation ids are re-derived for it.
            .map(|candidate| candidate.message(MessageId::new(), chat.id, turn_id));
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
                reasoning: MessageReasoning::default(),
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
    pub(crate) async fn ensure_durable_lease_current(&self, turn_id: TurnId) -> Result<()> {
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

    pub(crate) async fn accept_server_call_retry(
        &self,
        call: &ToolCallRecord,
    ) -> Result<AcceptedServerCall> {
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

    pub(crate) async fn resolve_server_call_retry(
        &self,
        chat_id: ChatId,
        turn_id: TurnId,
        call_id: CallId,
        resolution: &ToolCallResolution,
        preview: Option<&ToolResultPreview>,
    ) -> Result<ResolveToolCallOutcome> {
        let resolved_at = Utc::now();
        let Some(lease_token) = self.durable_steer_lease else {
            return self
                .store
                .resolve_server_tool_call_with_artifacts(call_id, resolution, resolved_at, preview)
                .await;
        };
        loop {
            match self
                .store
                .resolve_claimed_server_tool_call_with_artifacts(
                    call_id,
                    chat_id,
                    turn_id,
                    lease_token,
                    Utc::now(),
                    resolution,
                    resolved_at,
                    preview,
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

    pub(crate) async fn abandon_inherited_server_call_retry(
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

    pub(crate) async fn durable_generation_revision(&self, turn_id: TurnId) -> Result<Option<i64>> {
        match self.durable_steer_lease {
            Some(lease_token) => self
                .durable_turn_revision_retry(turn_id, lease_token)
                .await
                .map(Some),
            None => Ok(None),
        }
    }

    pub(crate) async fn durable_turn_revision_retry(
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

    pub(crate) async fn list_durable_steers_retry(
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

    pub(crate) async fn apply_durable_steer_retry(
        &self,
        turn_id: TurnId,
        lease_token: uuid::Uuid,
        steer_id: crate::id::TurnSteerId,
        attempt_event_ordinal: i32,
        preceding_assistant: Option<&Message>,
        preceding_citations: &[AssistantCitationInput],
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

    pub(crate) async fn wait_for_durable_store_retry(&self, turn_id: TurnId) -> Result<()> {
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
    pub(crate) fn reasoning_origin(&self) -> ReasoningOrigin {
        ReasoningOrigin {
            provider: self.config.provider.clone(),
            model: self.config.model.clone(),
        }
    }

    pub(crate) async fn persist(
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
                llm_content: None,
                reasoning: MessageReasoning::default(),
                created_at: Utc::now(),
            })
            .await?;
        Ok(id)
    }

    pub(crate) async fn persist_assistant(
        &self,
        chat_id: crate::id::ChatId,
        turn_id: TurnId,
        candidate: &AssistantCandidate,
    ) -> Result<MessageId> {
        let message = candidate.message(candidate.message_id, chat_id, turn_id);
        self.append_assistant_exact_retry(&message, &candidate.citations)
            .await?;
        Ok(message.id)
    }

    pub(crate) async fn append_assistant_exact_retry(
        &self,
        message: &Message,
        citations: &[AssistantCitationInput],
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
}
