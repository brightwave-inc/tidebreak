//! One live internal-engine session: the chat agent loop under a durable
//! turn lease.
//!
//! `run_turn` drives [`LegDriver::run_turn`] for the row the session worker
//! already inserted and claimed. Client waits still end the leg and drop the
//! lease so the claim scan can pick the turn up again.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::Utc;
use tokio::sync::Semaphore;

use tidebreak_core::db::DbStore;
use tidebreak_core::storage::DecidePlanOutcome;
use tidebreak_core::{
    chat_journal, AcceptTurnSteerOutcome, AnswerUserQuestions, AnswerUserQuestionsOutcome,
    AnswerUserQuestionsRequest, CallId, ChatId, CodeApprovalKind, CodeSessionId, CodeTurnStatus,
    DecidePlanRequest, OwnerId, PermissionMode, PlanDecision, PlanDecisionChoice,
    ToolApprovalStatus, TurnId, TurnSteerId, DEFAULT_ACCEPTED_PLAN_MODE,
};
use tidebreak_harness::{
    ApprovalDecision, HarnessApprovalRef, HarnessError, HarnessSession, ResumeInput, SessionSpec,
    TurnInput, TurnOutcome,
};

use crate::code::bus::CodeEventBus;
use crate::engine::internal::leg::{LegDriver, LegDriverOutcome};
use crate::state::AppState;

pub(super) struct InternalSession {
    state: AppState,
    db: Arc<DbStore>,
    #[allow(dead_code)]
    bus: Arc<CodeEventBus>,
    concurrency: Arc<Semaphore>,
    driver: LegDriver,
    owner: OwnerId,
    #[allow(dead_code)]
    session_id: CodeSessionId,
    chat_id: ChatId,
    active: Mutex<Option<TurnId>>,
    /// Tool approvals acknowledged through [`HarnessSession::decide`].
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
        concurrency: Arc<Semaphore>,
        driver: LegDriver,
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
        let session = Self {
            state,
            db,
            bus,
            concurrency,
            driver,
            owner: spec.owner,
            session_id: spec.session_id,
            chat_id,
            active: Mutex::new(None),
            decided: Mutex::new(HashSet::new()),
        };
        session.restore_parked_turn().await?;
        Ok(session)
    }

    /// Re-attach a waiting turn that survived a worker restart.
    ///
    /// `launch` always starts with no in-memory turn. A durable park still
    /// names the checkpoint on the turn row, so you put that turn back on
    /// `active` before `resume_turn` runs. `last_seq` is the journal tail
    /// so you do not replay the park that already landed.
    async fn restore_parked_turn(&self) -> Result<(), HarnessError> {
        let Some(turn) =
            tidebreak_core::db::code::get_open_turn(&self.db, &self.owner, self.session_id)
                .await
                .map_err(store_error)?
        else {
            return Ok(());
        };
        if turn.status != CodeTurnStatus::Waiting || turn.park_ref.is_none() {
            return Ok(());
        }
        *self.active.lock().expect("active turn") = Some(TurnId(turn.id.0));
        Ok(())
    }

    fn active_turn(&self) -> Option<TurnId> {
        *self.active.lock().expect("active turn")
    }

    fn map_leg_outcome(turn_id: TurnId, outcome: LegDriverOutcome) -> TurnOutcome {
        match outcome {
            LegDriverOutcome::Completed(_) | LegDriverOutcome::Cancelled(_) => TurnOutcome::Clean,
            LegDriverOutcome::WaitingForClient(_) | LegDriverOutcome::WaitingForAgentRun(_) => {
                // Client waits keep the lease-release shape until D4b: the
                // leg ends, the lease drops, and the turn returns to the
                // claim scan. Do not pin the worker on the wait.
                TurnOutcome::Clean
            }
            LegDriverOutcome::Resuming(_) => TurnOutcome::Clean,
            LegDriverOutcome::Failed(_) | LegDriverOutcome::LeaseLost(_) => {
                TurnOutcome::Incomplete {
                    detail: format!("turn {turn_id} did not complete"),
                }
            }
        }
    }

    /// Settle a parked continuation durably. Idempotent: the decision may
    /// already have landed through [`HarnessSession::decide`] on the leg
    /// that parked, or through the chat route, whose settlement the worker
    /// resumed this leg from; a row that is no longer pending is done.
    async fn settle_park(
        &self,
        call_id: CallId,
        decision: &ApprovalDecision,
    ) -> Result<(), HarnessError> {
        let settled = tidebreak_core::db::code::get_approval(
            &self.db,
            &self.owner,
            chat_journal::approval_id_of(call_id),
        )
        .await
        .map_err(store_error)?
        .is_some_and(|approval| !approval.state.is_pending());
        if settled {
            self.state.turn_job_wake.notify_one();
            return Ok(());
        }
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
                        completion_event,
                        resolution,
                        ..
                    } => {
                        let sender = self.state.events.sender(self.chat_id);
                        let _ = sender.send_row(*resolution);
                        let _ = sender.send(*completion_event);
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
                        completion_event,
                        resolution,
                        ..
                    } => {
                        let sender = self.state.events.sender(self.chat_id);
                        let _ = sender.send_row(*resolution);
                        let _ = sender.send(*completion_event);
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

    /// Acknowledge a tool decision the session decision route delivers.
    ///
    /// The route claimed the row and settles it once this returns, through
    /// the same operation every surface settles with; the settlement is
    /// what writes the standing grant an approve-with-grant names, in its
    /// own transaction. The engine's part is to check the card is this
    /// conversation's, still open, and offers the rung — the internal engine
    /// declares `standing_grants`, and this is where it honors them. The
    /// agent loop then reads the settled row and continues.
    async fn decide_tool_call(
        &self,
        call_id: CallId,
        decision: &ApprovalDecision,
    ) -> Result<(), HarnessError> {
        let current = self
            .state
            .store
            .get_tool_call_approval(call_id)
            .await
            .map_err(store_error)?
            .filter(|approval| approval.chat_id == self.chat_id)
            .ok_or_else(|| {
                HarnessError::ApprovalWaiterMissing(format!(
                    "tool call {call_id} is not waiting on a decision"
                ))
            })?;
        if current.status != ToolApprovalStatus::Pending {
            return Err(HarnessError::ApprovalWaiterMissing(format!(
                "tool call {call_id} is not waiting on a decision"
            )));
        }
        match decision {
            ApprovalDecision::Approve => {
                if !current.kind.is_approvable() {
                    return Err(HarnessError::DecisionUnsupported(format!(
                        "tool call {call_id} cannot be approved"
                    )));
                }
            }
            ApprovalDecision::Deny { .. } => {}
            ApprovalDecision::ApproveWithGrant { scope } => {
                if !current.kind.is_approvable() {
                    return Err(HarnessError::DecisionUnsupported(format!(
                        "tool call {call_id} cannot be approved"
                    )));
                }
                let offered = tidebreak_core::db::code::get_approval(
                    &self.db,
                    &self.owner,
                    chat_journal::approval_id_of(call_id),
                )
                .await
                .map_err(store_error)?
                .map(|approval| match approval.kind {
                    CodeApprovalKind::ToolUse { offered_grants, .. } => offered_grants,
                    _ => Vec::new(),
                })
                .unwrap_or_default();
                if !offered.contains(scope) {
                    return Err(HarnessError::DecisionUnsupported(
                        "that grant scope is not available for this call".into(),
                    ));
                }
            }
            ApprovalDecision::Answers { .. } | ApprovalDecision::PlanDecision { .. } => {
                return Err(HarnessError::DecisionUnsupported(
                    "a tool approval takes approve, deny, or approve with a grant".into(),
                ));
            }
        }
        self.decided.lock().expect("decided").insert(call_id);
        Ok(())
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
        let _permit = self.concurrency.acquire().await.map_err(|_| {
            HarnessError::Other("internal engine concurrency semaphore closed".into())
        })?;
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
        let turn_id = input.turn_id.map(|id| TurnId(id.0)).ok_or_else(|| {
            HarnessError::Other("the internal engine requires the host to name the turn".into())
        })?;
        let Some(turn) = self
            .state
            .store
            .get_turn(turn_id)
            .await
            .map_err(store_error)?
        else {
            return Err(HarnessError::Other(format!(
                "turn {turn_id} was not claimed before the leg"
            )));
        };
        let Some(lease_token) = turn.lease_token else {
            return Err(HarnessError::Other(format!("turn {turn_id} has no lease")));
        };
        *self.active.lock().expect("active turn") = Some(turn_id);
        let outcome = self
            .driver
            .run_turn(turn, lease_token)
            .await
            .map_err(|error| HarnessError::Other(error.to_string()))?;
        *self.active.lock().expect("active turn") = None;
        Ok(Self::map_leg_outcome(turn_id, outcome))
    }

    async fn resume_turn(
        &self,
        park_ref: String,
        input: ResumeInput,
    ) -> Result<TurnOutcome, HarnessError> {
        if self.active_turn().is_none() {
            self.restore_parked_turn().await?;
        }
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
        self.settle_park(call_id, &decision).await?;
        let Some(turn) = self
            .state
            .store
            .get_turn(turn_id)
            .await
            .map_err(store_error)?
        else {
            return Err(HarnessError::Other(format!(
                "turn {turn_id} was not claimed before the resume"
            )));
        };
        let Some(lease_token) = turn.lease_token else {
            return Err(HarnessError::Other(format!("turn {turn_id} has no lease")));
        };
        let outcome = self
            .driver
            .run_turn(turn, lease_token)
            .await
            .map_err(|error| HarnessError::Other(error.to_string()))?;
        *self.active.lock().expect("active turn") = None;
        Ok(Self::map_leg_outcome(turn_id, outcome))
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
                        .get_turn(turn_id)
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
