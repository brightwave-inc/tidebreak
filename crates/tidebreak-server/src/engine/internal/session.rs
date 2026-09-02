//! One live internal-engine session: a conversation the chat turn lane runs,
//! watched and steered through the adapter contract.
//!
//! The lane journals its turn straight into the session's code journal
//! (decision 0048 step 5), so the engine has nothing to translate. It follows
//! that journal for the turn it admitted and hands the session worker the
//! few facts the journal does not carry on its own: the approval rows a
//! consent card or a parked continuation needs, and the terminal outcome
//! that closes the worker's turn row.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::Utc;
use tokio::sync::broadcast;
use tracing::debug;

use tidebreak_core::db::DbStore;
use tidebreak_core::storage::DecidePlanOutcome;
use tidebreak_core::{
    chat_journal, AcceptTurnOutcome, AcceptTurnSteerOutcome, AnswerUserQuestions,
    AnswerUserQuestionsOutcome, AnswerUserQuestionsRequest, ApprovalDecision as ChatDecision,
    BeginTurnAdmissionOutcome, CallId, ChatId, CodeApprovalKind, CodeEvent, CodeSessionId,
    DecidePlanRequest, GrantScope, OwnerId, PermissionMode, PlanDecision, PlanDecisionChoice,
    ReservedTurnAcceptanceOutcome, SequencedCodeEvent, ToolActionPreview, TurnAdmissionRequest,
    TurnId, TurnSteerId, DEFAULT_ACCEPTED_PLAN_MODE, MAX_TOOL_SUMMARY_CHARS,
};
use tidebreak_harness::{
    ApprovalDecision, HarnessApprovalRef, HarnessError, HarnessEvent, HarnessEventSink,
    HarnessSession, ParkWait, ResumeInput, SessionSpec, TurnInput, TurnOutcome,
};

use crate::approvals::ResolveApprovalOutcome;
use crate::code::bus::{CodeEventBus, CodeLiveEvent};
use crate::state::AppState;

/// How long one admission reservation may sit before the engine gives up.
const ADMISSION_LEASE: chrono::Duration = chrono::Duration::seconds(30);

/// How many journal rows one catch-up read takes at a time.
const CATCH_UP_PAGE: u64 = 512;

/// The turn the engine is driving right now, and how far along the journal
/// it has read.
struct ActiveTurn {
    turn_id: TurnId,
    /// Last journal sequence handled, so a resume or a lagged bus
    /// subscription picks up exactly after it.
    last_seq: i64,
    /// Whether this turn's own `TurnStarted` has been seen; events before it
    /// belong to an earlier turn's tail.
    started: bool,
}

/// A parked continuation the engine answers with a store read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lookup {
    /// `ask_user_questions` parked the turn; the questions live in the store.
    Questions { call_id: CallId },
    /// `exit_plan_mode` parked the turn; the plan body lives in the store.
    Plan { call_id: CallId },
}

pub(super) struct InternalSession {
    state: AppState,
    db: Arc<DbStore>,
    bus: Arc<CodeEventBus>,
    owner: OwnerId,
    session_id: CodeSessionId,
    chat_id: ChatId,
    sink: Arc<dyn HarnessEventSink>,
    active: Mutex<Option<ActiveTurn>>,
    /// Tool approvals decided through [`HarnessSession::decide`], so the
    /// matching decision row on the journal is not re-reported as an
    /// engine-observed decision.
    decided: Mutex<HashSet<CallId>>,
}

fn store_error(error: tidebreak_core::AgentError) -> HarnessError {
    HarnessError::Other(format!("engine store: {error}"))
}

impl InternalSession {
    pub(super) async fn launch(
        state: AppState,
        db: Arc<DbStore>,
        bus: Arc<CodeEventBus>,
        spec: SessionSpec,
    ) -> Result<Self, HarnessError> {
        // The session row is the conversation row (decision 0048 step 5):
        // the runtime created it, and the engine follows the spec's posture
        // and model on every launch.
        let chat_id = ChatId(spec.session_id.0);
        if state
            .store
            .get_chat(chat_id)
            .await
            .map_err(store_error)?
            .is_none()
        {
            return Err(match spec.resume_ref.as_deref() {
                Some(resume) => {
                    HarnessError::ResumeLost(format!("conversation {resume} no longer exists"))
                }
                None => HarnessError::Other(format!(
                    "session {} has no conversation row",
                    spec.session_id
                )),
            });
        }
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
        // A session the code runtime created has no coordinator run yet;
        // the turn lane admits nothing without one.
        state
            .store
            .ensure_foreground_agent_run(chat_id)
            .await
            .map_err(store_error)?;
        Ok(Self {
            state,
            db,
            bus,
            owner: spec.owner,
            session_id: spec.session_id,
            chat_id,
            sink: spec.sink,
            active: Mutex::new(None),
            decided: Mutex::new(HashSet::new()),
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

    /// The newest journal sequence, so a turn admitted next is read from
    /// its first row.
    async fn journal_tail(&self) -> Result<i64, HarnessError> {
        Ok(
            tidebreak_core::db::code::list_recent_events(&self.db, &self.owner, self.session_id, 1)
                .await
                .map_err(store_error)?
                .first()
                .map_or(0, |event| event.seq),
        )
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

    /// Follow the session's journal for `turn_id` until it reaches a
    /// terminal event or a durable park.
    ///
    /// The live subscription is the wake-up; the durable journal is the
    /// truth. A lagged subscription re-reads from the last handled
    /// sequence, so nothing the lane journaled is skipped.
    async fn watch(
        &self,
        turn_id: TurnId,
        live: &mut broadcast::Receiver<CodeLiveEvent>,
    ) -> Result<TurnOutcome, HarnessError> {
        loop {
            loop {
                let after = self.last_seq();
                let missed = tidebreak_core::db::code::list_events_from(
                    &self.db,
                    &self.owner,
                    self.session_id,
                    after,
                    CATCH_UP_PAGE,
                )
                .await
                .map_err(store_error)?;
                let page = missed.len() as u64;
                for event in missed {
                    if let Some(outcome) = self.handle(turn_id, event).await? {
                        return Ok(outcome);
                    }
                }
                if page < CATCH_UP_PAGE {
                    break;
                }
            }
            loop {
                match live.recv().await {
                    Ok(CodeLiveEvent {
                        seq: Some(seq),
                        event,
                        ..
                    }) => {
                        if let Some(outcome) = self
                            .handle(turn_id, SequencedCodeEvent { seq, event })
                            .await?
                        {
                            return Ok(outcome);
                        }
                    }
                    // Live-only frames hold no row; the journal is the truth.
                    Ok(CodeLiveEvent { seq: None, .. }) => {}
                    Err(broadcast::error::RecvError::Lagged(dropped)) => {
                        debug!(
                            session = %self.session_id,
                            dropped,
                            "internal engine re-reads the journal after a lagged subscription"
                        );
                        break;
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        return Err(HarnessError::Other(
                            "the session event bus closed under a running turn".into(),
                        ));
                    }
                }
            }
        }
    }

    /// Act on one journaled row. `Some` closes the leg.
    async fn handle(
        &self,
        turn_id: TurnId,
        sequenced: SequencedCodeEvent,
    ) -> Result<Option<TurnOutcome>, HarnessError> {
        let SequencedCodeEvent { seq, event } = sequenced;
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
                CodeEvent::TurnStarted { turn_id: started } if started.0 == turn_id.0 => {
                    active.started = true;
                }
                _ if !active.started => return Ok(None),
                _ => {}
            }
        }
        let closed = match event {
            CodeEvent::ToolApprovalRequired {
                call_id,
                tool_name,
                grant_scopes,
                preview,
                ..
            } => {
                self.emit(approval_requested(
                    &Self::parse_call_id(&call_id)?,
                    serde_json::Value::Null,
                    tool_use_kind(&tool_name, preview, grant_scopes),
                ))
                .await;
                false
            }
            CodeEvent::ToolApprovalDecided { call_id, approved } => {
                let call_id = Self::parse_call_id(&call_id)?;
                let delivered = self.decided.lock().expect("decided").remove(&call_id);
                if !delivered {
                    // Decided on the engine's own channel — a standing grant
                    // or the auto-approval judge — so the row the worker
                    // minted settles from this report.
                    self.emit(approval_resolved(&call_id, observed_decision(approved)))
                        .await;
                }
                false
            }
            CodeEvent::QuestionsAsked { call_id, .. } => {
                let lookup = Lookup::Questions {
                    call_id: Self::parse_call_id(&call_id)?,
                };
                return Ok(Some(self.park(lookup).await?));
            }
            CodeEvent::PlanProposed { call_id, .. } => {
                let lookup = Lookup::Plan {
                    call_id: Self::parse_call_id(&call_id)?,
                };
                return Ok(Some(self.park(lookup).await?));
            }
            // The terminal rows are already journaled; the worker still
            // needs to hear them to close its turn row with the usage.
            CodeEvent::TurnCompleted { usage, .. } | CodeEvent::TurnRefused { usage, .. } => {
                self.emit(HarnessEvent::TurnCompleted { usage }).await;
                true
            }
            CodeEvent::TurnFailed { error, .. } => {
                self.emit(HarnessEvent::TurnFailed { error }).await;
                true
            }
            CodeEvent::TurnInterrupted { usage: Some(_) } => {
                self.emit(HarnessEvent::TurnInterrupted).await;
                true
            }
            _ => false,
        };
        if closed {
            *self.active.lock().expect("active turn") = None;
            return Ok(Some(TurnOutcome::Clean));
        }
        Ok(None)
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
        self.emit(approval_requested(&call_id, raw, kind)).await;
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
        let Some(chat_decision) = chat_decision(decision) else {
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
        // Subscribe before admission so the turn's first row cannot slip
        // between the two.
        let (mut live, _tail) = self.bus.attach(self.session_id);
        let after = self.journal_tail().await?;
        let turn_id = self.submit(&input.text).await?;
        *self.active.lock().expect("active turn") = Some(ActiveTurn {
            turn_id,
            last_seq: after,
            started: false,
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
        let (mut live, _tail) = self.bus.attach(self.session_id);
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

    /// Hand the lane a mid-turn message. The lane journals the steer itself,
    /// so nothing is emitted here.
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

/// A tool approval as the adapter states it: the exact server-built preview
/// and the grant ladder the engine offers, or a plain summary when the call
/// could not be described (no ladder is offered without a description).
fn tool_use_kind(
    tool_name: &str,
    preview: Option<ToolActionPreview>,
    grant_scopes: Vec<GrantScope>,
) -> CodeApprovalKind {
    match preview {
        // The model's own narration never reaches a consent card (decision
        // 0018): the card renders the literal action, and a call that could
        // describe itself to a decision could describe itself favourably.
        Some(preview) => CodeApprovalKind::ToolUse {
            preview: preview.without_summary(),
            offered_grants: grant_scopes,
        },
        None => CodeApprovalKind::Other {
            summary: chat_journal::bounded(tool_name, MAX_TOOL_SUMMARY_CHARS),
        },
    }
}

/// The decision the engine observed on its own channel, in adapter terms.
fn observed_decision(approved: bool) -> ApprovalDecision {
    if approved {
        ApprovalDecision::Approve
    } else {
        ApprovalDecision::Deny { feedback: None }
    }
}

/// The chat-side decision for an adapter decision on a tool approval.
///
/// Answers and plan decisions never reach here: they settle a parked
/// continuation through its own store operation.
fn chat_decision(decision: &ApprovalDecision) -> Option<ChatDecision> {
    match decision {
        ApprovalDecision::Approve | ApprovalDecision::ApproveWithGrant { .. } => {
            Some(ChatDecision::Approve)
        }
        ApprovalDecision::Deny { feedback } => Some(ChatDecision::Reject {
            reason: feedback
                .as_deref()
                .filter(|feedback| !feedback.trim().is_empty())
                .map_or_else(
                    || tidebreak_core::ToolApproval::DEFAULT_REJECT_REASON.to_owned(),
                    |feedback| {
                        chat_journal::bounded(
                            feedback,
                            tidebreak_core::ToolApproval::MAX_REASON_BYTES,
                        )
                    },
                ),
        }),
        ApprovalDecision::Answers { .. } | ApprovalDecision::PlanDecision { .. } => None,
    }
}

fn approval_requested(
    call_id: &CallId,
    raw: serde_json::Value,
    kind: CodeApprovalKind,
) -> HarnessEvent {
    HarnessEvent::ApprovalRequested {
        harness_ref: HarnessApprovalRef::engine(call_id.to_string()),
        raw,
        kind: Some(kind),
    }
}

fn approval_resolved(call_id: &CallId, decision: ApprovalDecision) -> HarnessEvent {
    HarnessEvent::ApprovalResolved {
        harness_ref: HarnessApprovalRef::engine(call_id.to_string()),
        decision,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_undescribed_call_offers_no_grant_ladder() {
        let kind = tool_use_kind("mystery", None, vec![GrantScope::WholeTool]);
        assert_eq!(
            kind,
            CodeApprovalKind::Other {
                summary: "mystery".into()
            }
        );
    }

    #[test]
    fn a_deny_without_feedback_carries_the_default_reason() {
        let ChatDecision::Reject { reason } =
            chat_decision(&ApprovalDecision::Deny { feedback: None }).unwrap()
        else {
            panic!("deny maps to reject");
        };
        assert_eq!(reason, tidebreak_core::ToolApproval::DEFAULT_REJECT_REASON);
        assert!(chat_decision(&ApprovalDecision::PlanDecision {
            approve: true,
            feedback: None
        })
        .is_none());
    }
}
