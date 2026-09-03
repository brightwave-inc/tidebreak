//! One live internal-engine session: the chat agent loop under a durable
//! turn lease.
//!
//! `run_turn` drives [`LegDriver::run_turn`] for the row the session worker
//! already inserted and claimed. Every durable wait returns through the
//! adapter park contract, so the session worker stays attached until resume.

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
    ToolApprovalStatus, TurnId, TurnParkWait, TurnSteerId, DEFAULT_ACCEPTED_PLAN_MODE,
};
use tidebreak_harness::{
    ApprovalDecision, HarnessApprovalRef, HarnessError, HarnessSession, ParkWait, ResumeInput,
    SessionSpec, TurnInput, TurnOutcome,
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
    active: Mutex<Option<ActiveTurn>>,
    /// Tool approvals acknowledged through [`HarnessSession::decide`].
    decided: Mutex<HashSet<CallId>>,
}

#[derive(Clone)]
struct ActiveTurn {
    turn_id: TurnId,
    park: Option<ActivePark>,
}

#[derive(Clone)]
struct ActivePark {
    park_ref: String,
    waiting_on: ParkWait,
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
        if !matches!(
            turn.status,
            CodeTurnStatus::Waiting | CodeTurnStatus::Resuming
        ) {
            return Ok(());
        }
        let (Some(park_ref), Some(wait)) = (turn.park_ref, turn.park_wait) else {
            return Ok(());
        };
        let waiting_on = match wait {
            TurnParkWait::Approval { call_id } => ParkWait::Approval { call_id },
            TurnParkWait::ClientToolCall { call_id } => ParkWait::ClientToolCall { call_id },
            TurnParkWait::AgentRuns { run_ids } => ParkWait::AgentRuns { run_ids },
        };
        *self.active.lock().expect("active turn") = Some(ActiveTurn {
            turn_id: TurnId(turn.id.0),
            park: Some(ActivePark {
                park_ref,
                waiting_on,
            }),
        });
        Ok(())
    }

    fn active_turn(&self) -> Option<TurnId> {
        self.active
            .lock()
            .expect("active turn")
            .as_ref()
            .map(|active| active.turn_id)
    }

    fn active_park(&self) -> Option<ActivePark> {
        self.active
            .lock()
            .expect("active turn")
            .as_ref()
            .and_then(|active| active.park.clone())
    }

    fn map_and_release(&self, turn_id: TurnId, outcome: LegDriverOutcome) -> TurnOutcome {
        let mapped = Self::map_leg_outcome(turn_id, outcome);
        *self.active.lock().expect("active turn") = match &mapped {
            TurnOutcome::Parked {
                park_ref,
                waiting_on,
            } => Some(ActiveTurn {
                turn_id,
                park: Some(ActivePark {
                    park_ref: park_ref.clone(),
                    waiting_on: waiting_on.clone(),
                }),
            }),
            TurnOutcome::Clean | TurnOutcome::Incomplete { .. } => None,
        };
        mapped
    }

    fn map_leg_outcome(turn_id: TurnId, outcome: LegDriverOutcome) -> TurnOutcome {
        match outcome {
            LegDriverOutcome::Completed(_) | LegDriverOutcome::Cancelled(_) => TurnOutcome::Clean,
            LegDriverOutcome::WaitingForApproval { call_id, .. } => {
                let call_id = call_id.to_string();
                TurnOutcome::Parked {
                    park_ref: call_id.clone(),
                    waiting_on: ParkWait::Approval { call_id },
                }
            }
            LegDriverOutcome::WaitingForClient { call_id, .. } => {
                let call_id = call_id.to_string();
                TurnOutcome::Parked {
                    park_ref: call_id.clone(),
                    waiting_on: ParkWait::ClientToolCall { call_id },
                }
            }
            LegDriverOutcome::WaitingForAgentRun {
                call_id, run_ids, ..
            } => TurnOutcome::Parked {
                park_ref: call_id.to_string(),
                waiting_on: ParkWait::AgentRuns {
                    run_ids: run_ids.into_iter().map(|id| id.to_string()).collect(),
                },
            },
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

    fn validate_resume(
        park: &ActivePark,
        park_ref: &str,
        input: &ResumeInput,
    ) -> Result<(), HarnessError> {
        if park.park_ref != park_ref {
            return Err(HarnessError::Other(format!(
                "park {park_ref} does not match the active park {}",
                park.park_ref
            )));
        }
        match (&park.waiting_on, input) {
            (
                ParkWait::Approval { call_id: waiting },
                ResumeInput::ApprovalDecided {
                    call_id: decided, ..
                },
            ) if waiting == decided => Ok(()),
            (
                ParkWait::ClientToolCall { call_id: waiting },
                ResumeInput::ClientToolCompleted { call_id: completed },
            ) if waiting == completed => Ok(()),
            (
                ParkWait::AgentRuns { run_ids: waiting },
                ResumeInput::AgentRunsSettled { run_ids: settled },
            ) if waiting == settled => Ok(()),
            (ParkWait::Approval { call_id }, _) => Err(HarnessError::ApprovalBindingMismatch(
                format!("park {park_ref} waited on approval {call_id}"),
            )),
            (ParkWait::ClientToolCall { call_id }, _) => Err(HarnessError::Other(format!(
                "park {park_ref} waited on client tool call {call_id}"
            ))),
            (ParkWait::AgentRuns { run_ids }, _) => Err(HarnessError::Other(format!(
                "park {park_ref} waited on agent runs {}",
                run_ids.join(", ")
            ))),
        }
    }

    async fn claim_resuming_turn(
        &self,
        turn_id: TurnId,
    ) -> Result<(tidebreak_core::TurnRun, uuid::Uuid), HarnessError> {
        // The checkpoint drops the lease before it returns. Yield so its
        // transaction releases SQLite's write lock before this claim starts.
        tokio::task::yield_now().await;
        let mut claimed = None;
        let mut lease_token = uuid::Uuid::nil();
        for _ in 0..40 {
            lease_token = uuid::Uuid::new_v4();
            let now = Utc::now();
            match self
                .db
                .take_lease_on_turn(
                    turn_id,
                    lease_token,
                    now,
                    now + chrono::Duration::seconds(60),
                )
                .await
            {
                Ok(value) => {
                    claimed = Some(value);
                    break;
                }
                Err(error) if error.to_string().contains("database is locked") => {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                Err(error) => return Err(store_error(error)),
            }
        }
        let Some(Some(())) = claimed else {
            return Err(HarnessError::Other(format!(
                "could not claim a lease on turn {turn_id} before resume"
            )));
        };
        let mut turn = None;
        for _ in 0..40 {
            match self.state.store.get_turn(turn_id).await {
                Ok(Some(loaded)) => {
                    turn = Some(loaded);
                    break;
                }
                Ok(None) => {
                    return Err(HarnessError::Other(format!(
                        "turn {turn_id} was not claimed before the resume"
                    )));
                }
                Err(error) if error.to_string().contains("database is locked") => {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                Err(error) => return Err(store_error(error)),
            }
        }
        let Some(turn) = turn else {
            return Err(HarnessError::Other(format!(
                "turn {turn_id} was not readable after the resume claim"
            )));
        };
        if turn.lease_token != Some(lease_token) {
            return Err(HarnessError::Other(format!(
                "turn {turn_id} reclaimed a different lease"
            )));
        }
        Ok((turn, lease_token))
    }

    async fn drive_claimed_turn(
        &self,
        turn_id: TurnId,
        mut turn: tidebreak_core::TurnRun,
        mut lease_token: uuid::Uuid,
    ) -> Result<LegDriverOutcome, HarnessError> {
        loop {
            let outcome = self
                .driver
                .run_turn(turn, lease_token)
                .await
                .map_err(|error| HarnessError::Other(error.to_string()))?;
            match outcome {
                LegDriverOutcome::Resuming(resuming_id) if resuming_id == turn_id => {
                    (turn, lease_token) = self.claim_resuming_turn(turn_id).await?;
                }
                LegDriverOutcome::Resuming(resuming_id) => {
                    return Err(HarnessError::Other(format!(
                        "turn {turn_id} returned a resume for {resuming_id}"
                    )));
                }
                outcome => return Ok(outcome),
            }
        }
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
        *self.active.lock().expect("active turn") = Some(ActiveTurn {
            turn_id,
            park: None,
        });
        let outcome = self.drive_claimed_turn(turn_id, turn, lease_token).await?;
        Ok(self.map_and_release(turn_id, outcome))
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
        let Some(active_park) = self.active_park() else {
            return Err(HarnessError::Other(format!(
                "turn {turn_id} has no active park for {park_ref}"
            )));
        };
        let _call_id = Self::parse_call_id(&park_ref)?;
        Self::validate_resume(&active_park, &park_ref, &input)?;
        // The dependency settled the row and dropped the lease. This worker
        // takes a fresh claim before it resumes the internal lane.
        let (turn, lease_token) = self.claim_resuming_turn(turn_id).await?;
        let outcome = self.drive_claimed_turn(turn_id, turn, lease_token).await?;
        Ok(self.map_and_release(turn_id, outcome))
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

#[cfg(test)]
mod tests {
    use super::*;
    use tidebreak_core::AgentRunId;

    #[test]
    fn client_and_agent_waits_map_to_adapter_parks() {
        let turn_id = TurnId::new();
        let client_call = CallId::new();
        assert_eq!(
            InternalSession::map_leg_outcome(
                turn_id,
                LegDriverOutcome::WaitingForClient {
                    turn_id,
                    call_id: client_call,
                },
            ),
            TurnOutcome::Parked {
                park_ref: client_call.to_string(),
                waiting_on: ParkWait::ClientToolCall {
                    call_id: client_call.to_string(),
                },
            }
        );

        let wait_call = CallId::new();
        let run_a = AgentRunId::new();
        let run_b = AgentRunId::new();
        assert_eq!(
            InternalSession::map_leg_outcome(
                turn_id,
                LegDriverOutcome::WaitingForAgentRun {
                    turn_id,
                    call_id: wait_call,
                    run_ids: vec![run_a, run_b],
                },
            ),
            TurnOutcome::Parked {
                park_ref: wait_call.to_string(),
                waiting_on: ParkWait::AgentRuns {
                    run_ids: vec![run_a.to_string(), run_b.to_string()],
                },
            }
        );
    }

    #[test]
    fn resume_input_must_match_the_active_park() {
        let call_id = CallId::new().to_string();
        let client = ActivePark {
            park_ref: call_id.clone(),
            waiting_on: ParkWait::ClientToolCall {
                call_id: call_id.clone(),
            },
        };
        assert!(InternalSession::validate_resume(
            &client,
            &call_id,
            &ResumeInput::ClientToolCompleted {
                call_id: call_id.clone(),
            },
        )
        .is_ok());
        assert!(InternalSession::validate_resume(
            &client,
            &call_id,
            &ResumeInput::ClientToolCompleted {
                call_id: CallId::new().to_string(),
            },
        )
        .is_err());

        let run_a = AgentRunId::new().to_string();
        let run_b = AgentRunId::new().to_string();
        let agents = ActivePark {
            park_ref: CallId::new().to_string(),
            waiting_on: ParkWait::AgentRuns {
                run_ids: vec![run_a.clone(), run_b.clone()],
            },
        };
        assert!(InternalSession::validate_resume(
            &agents,
            &agents.park_ref,
            &ResumeInput::AgentRunsSettled {
                run_ids: vec![run_a.clone(), run_b.clone()],
            },
        )
        .is_ok());
        assert!(InternalSession::validate_resume(
            &agents,
            &agents.park_ref,
            &ResumeInput::AgentRunsSettled {
                run_ids: vec![run_b, run_a],
            },
        )
        .is_err());
        assert!(InternalSession::validate_resume(
            &agents,
            &call_id,
            &ResumeInput::AgentRunsSettled {
                run_ids: match agents.waiting_on.clone() {
                    ParkWait::AgentRuns { run_ids } => run_ids,
                    _ => unreachable!(),
                },
            },
        )
        .is_err());
    }
}
