//! One live internal-engine session: a conversation the chat turn lane runs,
//! watched and steered through the adapter contract.

use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::Utc;
use tokio::sync::broadcast;
use tracing::{debug, warn};

use tidebreak_core::storage::DecidePlanOutcome;
use tidebreak_core::{
    AcceptTurnOutcome, AcceptTurnSteerOutcome, AgentEvent, AnswerUserQuestions,
    AnswerUserQuestionsOutcome, AnswerUserQuestionsRequest, BeginTurnAdmissionOutcome, CallId,
    Chat, ChatId, CodeApprovalKind, DecidePlanRequest, NetworkPolicy, PermissionMode, PlanDecision,
    PlanDecisionChoice, ReservedTurnAcceptanceOutcome, SequencedEvent, TurnAdmissionRequest,
    TurnId, TurnSteerId, DEFAULT_ACCEPTED_PLAN_MODE,
};
use tidebreak_harness::{
    ApprovalDecision, HarnessApprovalRef, HarnessError, HarnessEvent, HarnessEventSink,
    HarnessSession, ParkWait, ResumeInput, SessionSpec, TurnInput, TurnOutcome,
};

use super::translate::{self, Lookup, Translated};
use crate::approvals::ResolveApprovalOutcome;
use crate::state::AppState;

/// How long one admission reservation may sit before the engine gives up.
const ADMISSION_LEASE: chrono::Duration = chrono::Duration::seconds(30);

/// The turn the engine is driving right now, and how far along the chat
/// journal it has translated.
struct ActiveTurn {
    turn_id: TurnId,
    /// Last chat journal sequence translated, so a resume or a lagged bus
    /// subscription picks up exactly after it.
    last_seq: i64,
    /// Whether this turn's own `TurnStarted` has been seen; events before it
    /// belong to an earlier turn's tail.
    started: bool,
    /// Assistant prose streamed since the last message boundary.
    prose: String,
}

pub(super) struct InternalSession {
    state: AppState,
    chat_id: ChatId,
    sink: Arc<dyn HarnessEventSink>,
    active: Mutex<Option<ActiveTurn>>,
    /// Tool approvals decided through [`HarnessSession::decide`], so the
    /// matching `ApprovalDecided` on the chat journal is not re-reported as
    /// an engine-observed decision.
    decided: Mutex<HashSet<CallId>>,
    /// `UserSteered` emissions owed to [`HarnessSession::steer`]; the chat
    /// journal's own echo of each is swallowed.
    steers_emitted: AtomicUsize,
}

fn store_error(error: tidebreak_core::AgentError) -> HarnessError {
    HarnessError::Other(format!("engine store: {error}"))
}

impl InternalSession {
    pub(super) async fn launch(state: AppState, spec: SessionSpec) -> Result<Self, HarnessError> {
        let chat_id = ChatId(spec.session_id.0);
        let existing = state.store.get_chat(chat_id).await.map_err(store_error)?;
        match (existing, spec.resume_ref.as_deref()) {
            (Some(_), _) => {
                // Relaunch: the session's posture and model are the spec's,
                // and the conversation follows them.
                state
                    .store
                    .update_chat_metadata(
                        chat_id,
                        None,
                        Some(spec.model.clone()),
                        Some(spec.reasoning_effort),
                        Some(Some(spec.permission_mode)),
                        None,
                    )
                    .await
                    .map_err(store_error)?;
            }
            (None, Some(resume)) => {
                return Err(HarnessError::ResumeLost(format!(
                    "conversation {resume} no longer exists"
                )));
            }
            (None, None) => {
                let chat = Chat {
                    id: chat_id,
                    project_id: None,
                    title: None,
                    model: spec.model.clone(),
                    reasoning_effort: spec.reasoning_effort,
                    permission_mode: Some(spec.permission_mode),
                    network_policy: NetworkPolicy::default(),
                    attachment_revision: 0,
                    root_attachments: Vec::new(),
                    created_at: Utc::now(),
                };
                state
                    .store
                    .create_engine_private_chat(&spec.owner, &chat)
                    .await
                    .map_err(store_error)?;
            }
        }
        Ok(Self {
            state,
            chat_id,
            sink: spec.sink,
            active: Mutex::new(None),
            decided: Mutex::new(HashSet::new()),
            steers_emitted: AtomicUsize::new(0),
        })
    }

    fn active_turn(&self) -> Option<TurnId> {
        self.active
            .lock()
            .expect("active turn")
            .as_ref()
            .map(|active| active.turn_id)
    }

    fn last_seq(&self) -> i64 {
        self.active
            .lock()
            .expect("active turn")
            .as_ref()
            .map_or(0, |active| active.last_seq)
    }

    async fn emit(&self, event: HarnessEvent) {
        self.sink.emit(event).await;
    }

    /// Flush streamed prose as one assistant message at a boundary.
    async fn flush_prose(&self) {
        let prose = {
            let mut active = self.active.lock().expect("active turn");
            match active.as_mut() {
                Some(active) if !active.prose.trim().is_empty() => {
                    std::mem::take(&mut active.prose)
                }
                Some(active) => {
                    active.prose.clear();
                    return;
                }
                None => return,
            }
        };
        self.emit(translate::assistant_message(&prose)).await;
    }

    /// Submit the user's message to the chat turn lane and return the turn
    /// it admitted.
    async fn submit(&self, text: &str) -> Result<TurnId, HarnessError> {
        let chat = self
            .state
            .store
            .get_chat(self.chat_id)
            .await
            .map_err(store_error)?
            .ok_or_else(|| {
                HarnessError::ResumeLost(format!("conversation {} no longer exists", self.chat_id))
            })?;
        let model =
            crate::routes::providers_models::resolve_executable_chat_model(&self.state, &chat)
                .await
                .map_err(|error| HarnessError::Other(error.message().to_owned()))?;
        let turn_id = TurnId::new();
        let request = TurnAdmissionRequest {
            id: turn_id,
            chat_id: self.chat_id,
            content: text.to_owned(),
            attachments: Vec::new(),
            file_attachments: Vec::new(),
            invoked_skills: Vec::new(),
            voice_input_used: false,
        };
        loop {
            let lease = match self
                .state
                .store
                .begin_turn_admission(&request, uuid::Uuid::new_v4(), ADMISSION_LEASE)
                .await
                .map_err(store_error)?
            {
                BeginTurnAdmissionOutcome::Acquired(lease) => lease,
                BeginTurnAdmissionOutcome::Pending { .. } => {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    continue;
                }
                // A fresh turn id cannot already be accepted or queued; the
                // lane says so only for a replayed identity.
                BeginTurnAdmissionOutcome::Accepted | BeginTurnAdmissionOutcome::Queued => {
                    return Ok(turn_id);
                }
                BeginTurnAdmissionOutcome::IdentityConflict => {
                    return Err(HarnessError::Other(format!(
                        "turn {turn_id} was reserved with different input"
                    )));
                }
            };
            match self
                .state
                .store
                .accept_reserved_turn_with_message_context(
                    lease,
                    self.chat_id,
                    &model,
                    text,
                    &[],
                    &[],
                    &[],
                    false,
                )
                .await
                .map_err(store_error)?
            {
                ReservedTurnAcceptanceOutcome::LeaseLost => continue,
                ReservedTurnAcceptanceOutcome::Outcome(outcome) => match *outcome {
                    AcceptTurnOutcome::Accepted(turn) | AcceptTurnOutcome::Existing(turn) => {
                        self.state.turn_job_wake.notify_one();
                        return Ok(turn.id);
                    }
                    AcceptTurnOutcome::IdentityConflict => {
                        let _ = self.state.store.release_turn_admission(lease).await;
                        return Err(HarnessError::Other(format!(
                            "turn {turn_id} was reserved with different input"
                        )));
                    }
                    AcceptTurnOutcome::ChatBusy(active) => {
                        let _ = self.state.store.release_turn_admission(lease).await;
                        return Err(HarnessError::Other(format!(
                            "the conversation is still running turn {}",
                            active.id
                        )));
                    }
                },
            }
        }
    }

    /// Follow the chat journal for `turn_id` until it reaches a terminal
    /// event or a durable park, translating as it goes.
    ///
    /// The live subscription is the wake-up; the durable journal is the
    /// truth. A lagged subscription re-reads from the last translated
    /// sequence, so nothing the lane journaled is skipped.
    async fn watch(
        &self,
        turn_id: TurnId,
        live: &mut broadcast::Receiver<SequencedEvent>,
    ) -> Result<TurnOutcome, HarnessError> {
        loop {
            let after = self.last_seq();
            let missed = self
                .state
                .store
                .list_events(self.chat_id, after)
                .await
                .map_err(store_error)?;
            for event in missed {
                if let Some(outcome) = self.handle(turn_id, event).await? {
                    return Ok(outcome);
                }
            }
            loop {
                match live.recv().await {
                    Ok(event) => {
                        if let Some(outcome) = self.handle(turn_id, event).await? {
                            return Ok(outcome);
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(dropped)) => {
                        debug!(
                            chat_id = %self.chat_id,
                            dropped,
                            "internal engine re-reads the journal after a lagged subscription"
                        );
                        break;
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        return Err(HarnessError::Other(
                            "the chat event bus closed under a running turn".into(),
                        ));
                    }
                }
            }
        }
    }

    /// Translate one journaled event. `Some` closes the leg.
    async fn handle(
        &self,
        turn_id: TurnId,
        sequenced: SequencedEvent,
    ) -> Result<Option<TurnOutcome>, HarnessError> {
        let SequencedEvent { seq, event } = sequenced;
        {
            let mut active = self.active.lock().expect("active turn");
            let Some(active) = active.as_mut() else {
                return Ok(None);
            };
            if seq <= active.last_seq {
                return Ok(None);
            }
            active.last_seq = seq;
            match &event {
                AgentEvent::TurnStarted { turn_id: started } if *started == turn_id => {
                    active.started = true;
                }
                _ if !active.started => return Ok(None),
                AgentEvent::TextDelta { text } => active.prose.push_str(text),
                _ => {}
            }
        }
        match self.translate(&event).await? {
            Translated::Emit(events) => {
                for event in events {
                    self.emit(event).await;
                }
            }
            Translated::Lookup(lookup) => {
                let park = self.park(lookup).await?;
                return Ok(Some(park));
            }
        }
        Ok(match event {
            AgentEvent::TurnCompleted { .. }
            | AgentEvent::TurnRefused { .. }
            | AgentEvent::TurnFailed { .. }
            | AgentEvent::TurnCancelled { .. } => {
                *self.active.lock().expect("active turn") = None;
                Some(TurnOutcome::Clean)
            }
            _ => None,
        })
    }

    async fn translate(&self, event: &AgentEvent) -> Result<Translated, HarnessError> {
        let emit = |events: Vec<HarnessEvent>| Ok(Translated::Emit(events));
        match event {
            AgentEvent::TurnStarted { .. } => emit(vec![HarnessEvent::TurnStarted]),
            AgentEvent::TextDelta { text } => {
                emit(vec![HarnessEvent::AssistantDelta { text: text.clone() }])
            }
            AgentEvent::ReasoningDelta { text } => {
                emit(vec![HarnessEvent::ReasoningDelta { text: text.clone() }])
            }
            // The lane discards the deltas it showed; the prose it kept is
            // already its own message, so the boundary lands here.
            AgentEvent::StreamInterrupted => {
                self.flush_prose().await;
                emit(Vec::new())
            }
            AgentEvent::ToolCallStarted { call_id, name } => {
                self.flush_prose().await;
                emit(vec![translate::tool_started(call_id, name)])
            }
            AgentEvent::ToolCallArgsDelta { .. } | AgentEvent::TaskPlanUpdated { .. } => {
                emit(Vec::new())
            }
            AgentEvent::ToolCallCompleted {
                call_id,
                output,
                action,
                ..
            } => emit(vec![translate::tool_completed(
                call_id,
                output,
                action.as_ref(),
            )]),
            AgentEvent::ApprovalRequired {
                call_id,
                tool_name,
                grant_scopes,
                preview,
                ..
            } => {
                self.flush_prose().await;
                emit(vec![translate::approval_requested(
                    call_id,
                    serde_json::Value::Null,
                    translate::tool_use_kind(tool_name, preview.clone(), grant_scopes.clone()),
                )])
            }
            AgentEvent::ApprovalDecided { call_id, approved } => {
                let delivered = self.decided.lock().expect("decided").remove(call_id);
                if delivered {
                    emit(Vec::new())
                } else {
                    // Decided on the engine's own channel — a standing grant
                    // or the auto-approval judge — so the row the worker
                    // minted settles from this report.
                    emit(vec![translate::approval_resolved(
                        call_id,
                        translate::observed_decision(*approved),
                    )])
                }
            }
            AgentEvent::UserQuestionsAsked { call_id, .. } => {
                self.flush_prose().await;
                Ok(Translated::Lookup(Lookup::Questions { call_id: *call_id }))
            }
            AgentEvent::PlanProposed { call_id, .. } => {
                self.flush_prose().await;
                Ok(Translated::Lookup(Lookup::Plan { call_id: *call_id }))
            }
            AgentEvent::UserSteered { content, .. } => {
                self.flush_prose().await;
                let owed = self.steers_emitted.load(Ordering::SeqCst);
                if owed > 0 {
                    self.steers_emitted.fetch_sub(1, Ordering::SeqCst);
                    emit(Vec::new())
                } else {
                    emit(vec![HarnessEvent::UserSteered {
                        text: content.clone(),
                    }])
                }
            }
            AgentEvent::TurnCompleted { usage, .. } => {
                self.flush_prose().await;
                emit(vec![HarnessEvent::TurnCompleted {
                    usage: translate::code_usage(*usage),
                }])
            }
            AgentEvent::TurnRefused { usage, refusal } => {
                self.flush_prose().await;
                emit(vec![
                    translate::refusal_notice(refusal),
                    HarnessEvent::TurnCompleted {
                        usage: translate::code_usage(*usage),
                    },
                ])
            }
            AgentEvent::TurnFailed { error } => {
                self.flush_prose().await;
                emit(vec![translate::failure(error)])
            }
            AgentEvent::TurnCancelled { .. } => {
                self.flush_prose().await;
                emit(vec![HarnessEvent::TurnInterrupted])
            }
            AgentEvent::ContextTruncated {
                original_tokens,
                fitted_tokens,
            } => emit(vec![translate::notice(&format!(
                "Context was trimmed from {original_tokens} to {fitted_tokens} tokens."
            ))]),
            AgentEvent::CompactionStarted => emit(vec![translate::notice(
                "Compacting the conversation to stay within the context window.",
            )]),
            AgentEvent::CompactionFinished { compacted } => emit(if *compacted {
                vec![translate::notice("Conversation compacted.")]
            } else {
                Vec::new()
            }),
            _ => {
                warn!(chat_id = %self.chat_id, "internal engine met an unmapped chat event");
                emit(Vec::new())
            }
        }
    }

    /// Publish the parked continuation as an approval and end the leg.
    async fn park(&self, lookup: Lookup) -> Result<TurnOutcome, HarnessError> {
        let (call_id, raw, kind) = match lookup {
            Lookup::Questions { call_id } => {
                let pending = self
                    .state
                    .store
                    .list_pending_user_questions(self.chat_id)
                    .await
                    .map_err(store_error)?
                    .into_iter()
                    .find(|pending| pending.call_id == call_id)
                    .ok_or_else(|| {
                        HarnessError::Other(format!("questions {call_id} are not pending"))
                    })?;
                (
                    call_id,
                    serde_json::Value::Null,
                    CodeApprovalKind::Questions {
                        questions: pending.questions,
                    },
                )
            }
            Lookup::Plan { call_id } => {
                let pending = self
                    .state
                    .store
                    .list_pending_plan_approvals(self.chat_id)
                    .await
                    .map_err(store_error)?
                    .into_iter()
                    .find(|pending| pending.call_id == call_id)
                    .ok_or_else(|| HarnessError::Other(format!("plan {call_id} is not pending")))?;
                // The plan body rides the raw payload: the approvals route
                // serves it, and the kind stays small enough for the
                // journal.
                (
                    call_id,
                    serde_json::json!({
                        "title": pending.title,
                        "plan": pending.plan,
                    }),
                    CodeApprovalKind::Plan {
                        proposed_mode: DEFAULT_ACCEPTED_PLAN_MODE,
                    },
                )
            }
        };
        self.emit(translate::approval_requested(&call_id, raw, kind))
            .await;
        Ok(TurnOutcome::Parked {
            park_ref: call_id.to_string(),
            waiting_on: ParkWait::Approval {
                call_id: call_id.to_string(),
            },
        })
    }

    /// Settle a parked continuation durably. Idempotent: the decision may
    /// already have landed through [`HarnessSession::decide`] on the leg
    /// that parked.
    async fn settle_park(
        &self,
        call_id: CallId,
        decision: &ApprovalDecision,
    ) -> Result<(), HarnessError> {
        match decision {
            ApprovalDecision::Answers { answers } => {
                let request = AnswerUserQuestionsRequest {
                    chat_id: self.chat_id,
                    call_id,
                    answers: AnswerUserQuestions {
                        answers: answers.clone(),
                        additional_user_context: None,
                    },
                };
                match self
                    .state
                    .store
                    .answer_user_questions(&request, Utc::now())
                    .await
                    .map_err(store_error)?
                {
                    AnswerUserQuestionsOutcome::Answered {
                        completion_event, ..
                    } => {
                        let _ = self
                            .state
                            .events
                            .sender(self.chat_id)
                            .send(*completion_event);
                        self.state.turn_job_wake.notify_one();
                        Ok(())
                    }
                    AnswerUserQuestionsOutcome::Existing(_) => Ok(()),
                    AnswerUserQuestionsOutcome::AnswerConflict => Err(HarnessError::Other(
                        "these questions were already answered differently".into(),
                    )),
                    AnswerUserQuestionsOutcome::InvalidAnswer => {
                        Err(HarnessError::DecisionUnsupported(
                            "the answers do not fit the questions".into(),
                        ))
                    }
                    AnswerUserQuestionsOutcome::Unavailable => Err(
                        HarnessError::ApprovalWaiterMissing(format!("questions {call_id}")),
                    ),
                }
            }
            ApprovalDecision::PlanDecision { approve, feedback } => {
                let request = DecidePlanRequest {
                    chat_id: self.chat_id,
                    call_id,
                    decision: PlanDecision {
                        decision: if *approve {
                            PlanDecisionChoice::Accept
                        } else {
                            PlanDecisionChoice::Reject
                        },
                        feedback: feedback.clone(),
                        // The posture change itself is the caller's
                        // `set_permission_mode`; naming the same mode here
                        // keeps the lane's own transition idempotent with it.
                        permission_mode: approve.then_some(DEFAULT_ACCEPTED_PLAN_MODE),
                    },
                };
                match self
                    .state
                    .store
                    .decide_plan(&request, Utc::now())
                    .await
                    .map_err(store_error)?
                {
                    DecidePlanOutcome::Decided {
                        completion_event, ..
                    } => {
                        let _ = self
                            .state
                            .events
                            .sender(self.chat_id)
                            .send(*completion_event);
                        self.state.turn_job_wake.notify_one();
                        Ok(())
                    }
                    DecidePlanOutcome::Existing(_) => Ok(()),
                    DecidePlanOutcome::DecisionConflict => Err(HarnessError::Other(
                        "this plan was already decided differently".into(),
                    )),
                    DecidePlanOutcome::InvalidDecision => Err(HarnessError::DecisionUnsupported(
                        "the plan decision is malformed".into(),
                    )),
                    DecidePlanOutcome::Unavailable => Err(HarnessError::ApprovalWaiterMissing(
                        format!("plan {call_id}"),
                    )),
                }
            }
            ApprovalDecision::Approve
            | ApprovalDecision::Deny { .. }
            | ApprovalDecision::ApproveWithGrant { .. } => Err(HarnessError::DecisionUnsupported(
                "a parked continuation takes answers or a plan decision".into(),
            )),
        }
    }

    /// Decide a tool approval on the chat lane's approval broker.
    async fn decide_tool_call(
        &self,
        call_id: CallId,
        decision: &ApprovalDecision,
    ) -> Result<(), HarnessError> {
        let Some(chat_decision) = translate::chat_decision(decision) else {
            return Err(HarnessError::DecisionUnsupported(
                "a tool approval takes approve, deny, or approve with a grant".into(),
            ));
        };
        self.decided.lock().expect("decided").insert(call_id);
        let outcome = match decision {
            ApprovalDecision::ApproveWithGrant { scope } => {
                self.state
                    .approvals
                    .resolve_with_scope(self.chat_id, call_id, scope.clone())
                    .await
            }
            _ => {
                self.state
                    .approvals
                    .resolve(self.chat_id, call_id, chat_decision)
                    .await
            }
        }
        .map_err(store_error)?;
        match outcome {
            ResolveApprovalOutcome::Resolved => Ok(()),
            ResolveApprovalOutcome::NotPending
            | ResolveApprovalOutcome::WrongChat
            | ResolveApprovalOutcome::DecisionConflict => {
                self.decided.lock().expect("decided").remove(&call_id);
                Err(HarnessError::ApprovalWaiterMissing(format!(
                    "tool call {call_id} is not waiting on a decision"
                )))
            }
            ResolveApprovalOutcome::NotApprovable => {
                self.decided.lock().expect("decided").remove(&call_id);
                Err(HarnessError::DecisionUnsupported(format!(
                    "tool call {call_id} cannot be approved"
                )))
            }
            ResolveApprovalOutcome::GrantNotAvailable => {
                self.decided.lock().expect("decided").remove(&call_id);
                Err(HarnessError::DecisionUnsupported(
                    "that grant scope is not available for this call".into(),
                ))
            }
        }
    }

    fn parse_call_id(raw: &str) -> Result<CallId, HarnessError> {
        raw.parse::<CallId>().map_err(|_| {
            HarnessError::ApprovalBindingMismatch(format!("{raw} is not an engine call id"))
        })
    }
}

#[async_trait]
impl HarnessSession for InternalSession {
    async fn run_turn(&self, input: TurnInput) -> Result<TurnOutcome, HarnessError> {
        if !input.images.is_empty() {
            return Err(HarnessError::Other(
                "image input is not available on the internal engine yet".into(),
            ));
        }
        if self.active_turn().is_some() {
            return Err(HarnessError::Other(
                "the internal engine is already running a turn".into(),
            ));
        }
        if input.model.is_some() || input.reasoning_effort.is_some() {
            self.state
                .store
                .update_chat_metadata(
                    self.chat_id,
                    None,
                    input.model.clone().map(Some),
                    input.reasoning_effort.map(Some),
                    None,
                    None,
                )
                .await
                .map_err(store_error)?;
        }
        // Subscribe before admission so the turn's first event cannot slip
        // between the two.
        let mut live = self.state.events.subscribe(self.chat_id);
        let after = self
            .state
            .store
            .list_events(self.chat_id, 0)
            .await
            .map_err(store_error)?
            .last()
            .map_or(0, |event| event.seq);
        let turn_id = self.submit(&input.text).await?;
        *self.active.lock().expect("active turn") = Some(ActiveTurn {
            turn_id,
            last_seq: after,
            started: false,
            prose: String::new(),
        });
        let outcome = self.watch(turn_id, &mut live).await;
        if !matches!(outcome, Ok(TurnOutcome::Parked { .. })) {
            *self.active.lock().expect("active turn") = None;
        }
        outcome
    }

    async fn resume_turn(
        &self,
        park_ref: String,
        input: ResumeInput,
    ) -> Result<TurnOutcome, HarnessError> {
        let Some(turn_id) = self.active_turn() else {
            return Err(HarnessError::Other(format!(
                "no parked turn to resume for {park_ref}"
            )));
        };
        let call_id = Self::parse_call_id(&park_ref)?;
        let ResumeInput::ApprovalDecided {
            call_id: decided,
            decision,
        } = input
        else {
            return Err(HarnessError::ParkResumeUnsupported);
        };
        if decided != park_ref {
            return Err(HarnessError::ApprovalBindingMismatch(format!(
                "park {park_ref} did not wait on {decided}"
            )));
        }
        let mut live = self.state.events.subscribe(self.chat_id);
        self.settle_park(call_id, &decision).await?;
        let outcome = self.watch(turn_id, &mut live).await;
        if !matches!(outcome, Ok(TurnOutcome::Parked { .. })) {
            *self.active.lock().expect("active turn") = None;
        }
        outcome
    }

    async fn decide(
        &self,
        approval: HarnessApprovalRef,
        decision: ApprovalDecision,
    ) -> Result<(), HarnessError> {
        let call_id = Self::parse_call_id(&approval.call_id)?;
        match decision {
            ApprovalDecision::Answers { .. } | ApprovalDecision::PlanDecision { .. } => {
                // A continuation decided before its park landed settles the
                // same durable row; the resume that follows finds it done.
                self.settle_park(call_id, &decision).await
            }
            ApprovalDecision::Approve
            | ApprovalDecision::Deny { .. }
            | ApprovalDecision::ApproveWithGrant { .. } => {
                self.decide_tool_call(call_id, &decision).await
            }
        }
    }

    async fn interrupt(&self) -> Result<(), HarnessError> {
        let Some(turn_id) = self.active_turn() else {
            return Ok(());
        };
        loop {
            match self
                .state
                .store
                .request_turn_cancellation_and_append_event(turn_id, Utc::now())
                .await
                .map_err(store_error)?
            {
                Some(resolution) => {
                    if let Some(event) = resolution.terminal_event {
                        let _ = self.state.events.sender(self.chat_id).send(event);
                    }
                    break;
                }
                None => {
                    // A heartbeat moved the row under this request; retry with
                    // fresh time unless the turn is gone.
                    let present = self
                        .state
                        .store
                        .get_turn_run(turn_id)
                        .await
                        .map_err(store_error)?
                        .is_some();
                    if !present {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            }
        }
        self.state.active_turns.cancel(self.chat_id, turn_id);
        self.state.turn_job_wake.notify_one();
        self.state.agent_run_wake.notify_one();
        self.state.queued_turn_wake.notify_one();
        Ok(())
    }

    async fn set_permission_mode(&self, mode: PermissionMode) -> Result<(), HarnessError> {
        self.state
            .store
            .update_chat_metadata(self.chat_id, None, None, None, Some(Some(mode)), None)
            .await
            .map_err(store_error)?;
        Ok(())
    }

    async fn steer(&self, text: String) -> Result<(), HarnessError> {
        let Some(turn_id) = self.active_turn() else {
            return Err(HarnessError::SteeringRejected("no turn is running".into()));
        };
        match self
            .state
            .store
            .accept_turn_steer_with_message_context(
                TurnSteerId::new(),
                turn_id,
                self.chat_id,
                &text,
                &[],
                false,
                false,
            )
            .await
            .map_err(store_error)?
        {
            AcceptTurnSteerOutcome::Accepted(_) | AcceptTurnSteerOutcome::Existing(_) => {
                self.state
                    .active_turns
                    .signal_steer(self.chat_id, turn_id, false);
                self.steers_emitted.fetch_add(1, Ordering::SeqCst);
                self.emit(HarnessEvent::UserSteered { text }).await;
                Ok(())
            }
            AcceptTurnSteerOutcome::TurnUnavailable => Err(HarnessError::SteeringRejected(
                "the turn is not taking guidance right now".into(),
            )),
            AcceptTurnSteerOutcome::IdentityConflict => Err(HarnessError::SteeringRejected(
                "the steer collided with another".into(),
            )),
        }
    }

    fn resume_ref(&self) -> Option<String> {
        Some(self.chat_id.to_string())
    }

    fn unrecognized_events(&self) -> u64 {
        0
    }

    async fn shutdown(self: Box<Self>) -> Result<(), HarnessError> {
        Ok(())
    }
}
