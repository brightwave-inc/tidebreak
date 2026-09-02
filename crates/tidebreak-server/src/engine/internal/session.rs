//! One live internal-engine session: a conversation the chat turn lane runs,
//! watched and steered through the adapter contract.
//!
//! The lane journals its turn straight into the session's code journal
//! (decision 0048 step 5), so the engine has nothing to translate. It follows
//! that journal for the turn it admitted and hands the session worker the
//! one fact the journal does not carry on its own: the terminal outcome that
//! closes the worker's turn row. The lane mints its own approval rows — a
//! consent card, a questions park, a plan park are each one `code_approval`
//! row whose id is the call id — so the engine parks on the row it finds and
//! decides through the same store operations the chat routes use.

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
    AnswerUserQuestionsOutcome, AnswerUserQuestionsRequest, ApprovalDecisionKind, Attention,
    AttentionSource, BeginTurnAdmissionOutcome, CallId, ChatId, CodeApprovalKind, CodeEvent,
    CodeSessionId, CodeTurnStatus, DecidePlanRequest, GrantLevel, GrantScope, ImageRef,
    InternalApprovalRequest, OwnerId, PermissionMode, PlanDecision, PlanDecisionChoice,
    ReservedTurnAcceptanceOutcome, SequencedCodeEvent, SequencedEvent, StandingGrant,
    ToolApprovalStatus, TurnAdmissionRequest, TurnId, TurnSteerId, DEFAULT_ACCEPTED_PLAN_MODE,
};
use tidebreak_harness::{
    ApprovalDecision, HarnessApprovalRef, HarnessError, HarnessEvent, HarnessEventSink,
    HarnessSession, ParkWait, ResumeInput, SessionSpec, TurnImage, TurnInput, TurnOutcome,
};

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

pub(super) struct InternalSession {
    state: AppState,
    db: Arc<DbStore>,
    bus: Arc<CodeEventBus>,
    owner: OwnerId,
    session_id: CodeSessionId,
    chat_id: ChatId,
    sink: Arc<dyn HarnessEventSink>,
    active: Mutex<Option<ActiveTurn>>,
    /// Tool approvals acknowledged through [`HarnessSession::decide`]. The
    /// session decision route settles those rows and journals the decision
    /// on the code bus; the chat's live channel hears it from here.
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
        let session = Self {
            state,
            db,
            bus,
            owner: spec.owner,
            session_id: spec.session_id,
            chat_id,
            sink: spec.sink,
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
        let last_seq = self.journal_tail().await?;
        *self.active.lock().expect("active turn") = Some(ActiveTurn {
            turn_id: TurnId(turn.id.0),
            last_seq,
            started: true,
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

    /// Publish turn images through chat's attachment model: sniff and bound
    /// them the way chat ingest does, put the bytes, and reserve the chat's
    /// authority to attach each id.
    async fn publish_turn_images(
        &self,
        images: Vec<TurnImage>,
    ) -> Result<Vec<ImageRef>, HarnessError> {
        let mut published = Vec::with_capacity(images.len());
        for image in images {
            let inspected = crate::routes::image_attachment::inspect_image_bytes(&image.bytes)
                .map_err(|error| HarnessError::Other(error.message().to_owned()))?;
            let _blob_write = self
                .state
                .blob_writes
                .acquire(inspected.blob_id)
                .await
                .map_err(store_error)?;
            self.state
                .blobs
                .put(inspected.blob_id, image.bytes)
                .await
                .map_err(store_error)?;
            if !self
                .state
                .store
                .publish_chat_image_scoped(&self.owner, self.chat_id, &inspected)
                .await
                .map_err(store_error)?
            {
                return Err(HarnessError::ResumeLost(format!(
                    "conversation {} no longer exists",
                    self.chat_id
                )));
            }
            published.push(inspected);
        }
        Ok(published)
    }

    /// Submit the user's message to the chat turn lane and return the turn
    /// it admitted.
    async fn submit(&self, text: &str, images: Vec<TurnImage>) -> Result<TurnId, HarnessError> {
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
        let attachments = self.publish_turn_images(images).await?;
        let turn_id = TurnId::new();
        let request = TurnAdmissionRequest {
            id: turn_id,
            chat_id: self.chat_id,
            content: text.to_owned(),
            attachments: attachments.iter().map(|image| image.blob_id).collect(),
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
                    &attachments,
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
            // The lane minted the row and journaled the request; the engine
            // only marks the session as waiting on the reader.
            CodeEvent::ApprovalRequested {
                request: Some(InternalApprovalRequest::ToolUse { .. }),
                ..
            } => {
                self.note_waiting("an approval is waiting").await;
                false
            }
            // A park: the row the lane minted is the one the worker waits
            // on, keyed by the call id the row's id is.
            CodeEvent::ApprovalRequested {
                approval_id,
                request:
                    Some(
                        InternalApprovalRequest::Questions { .. }
                        | InternalApprovalRequest::Plan { .. },
                    ),
            } => {
                self.note_waiting("the agent is waiting on you").await;
                let call_id = CallId(approval_id.0).to_string();
                return Ok(Some(TurnOutcome::Parked {
                    park_ref: call_id.clone(),
                    waiting_on: ParkWait::Approval { call_id },
                }));
            }
            CodeEvent::ApprovalRequested { request: None, .. } => false,
            CodeEvent::ApprovalResolved {
                approval_id,
                decision,
            } => {
                let call_id = CallId(approval_id.0);
                // A decision the session route delivered was settled and
                // journaled by the runtime on the code bus; the chat's live
                // channel hears it from here. Every other settlement was
                // published by the chat-side path that made it.
                let delivered = self.decided.lock().expect("decided").remove(&call_id);
                if delivered {
                    if let ApprovalDecisionKind::ApprovedWithGrant { scope } = &decision {
                        self.record_settled_grant(call_id, scope.clone()).await;
                    }
                    if let Ok(Some(event)) = chat_journal::chat_event(CodeEvent::ApprovalResolved {
                        approval_id,
                        decision,
                    }) {
                        let _ = self
                            .state
                            .events
                            .sender(self.chat_id)
                            .send_unmirrored(SequencedEvent { seq, event });
                    }
                }
                self.note_activity().await;
                false
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

    /// Mirror a grant the settlement wrote into the broker's in-memory
    /// cache, after the fact: the cache follows the store, never leads it.
    async fn record_settled_grant(&self, call_id: CallId, scope: GrantScope) {
        let Ok(Some(approval)) = self.state.store.get_tool_call_approval(call_id).await else {
            return;
        };
        let project_id = self
            .state
            .store
            .get_chat(self.chat_id)
            .await
            .ok()
            .flatten()
            .and_then(|chat| chat.project_id);
        if let Some(grant) = StandingGrant::scoped(
            GrantLevel::for_chat(self.chat_id, project_id),
            approval.tool_name,
            approval.kind,
            scope,
            Utc::now(),
        ) {
            self.state.approvals.standing_grants().record(grant);
        }
    }

    /// Mark the session as waiting on the reader, the way the worker does
    /// for an approval an external engine raised.
    async fn note_waiting(&self, prompt: &str) {
        let _ = crate::code::attention::note_activity(
            &self.db,
            &self.bus,
            &self.owner,
            self.session_id,
        )
        .await;
        let _ = crate::code::attention::apply_attention(
            &self.db,
            &self.bus,
            &self.owner,
            self.session_id,
            Attention::needs_you(prompt, AttentionSource::Structured),
            false,
        )
        .await;
    }

    async fn note_activity(&self) {
        let _ = crate::code::attention::note_activity(
            &self.db,
            &self.bus,
            &self.owner,
            self.session_id,
        )
        .await;
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
        let turn_id = self.submit(&input.text, input.images).await?;
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
