//! Approval records: listing, external approvals, and decisions.

use super::*;

/// Resolve a requested decision into the engine-channel decision, refusing
/// anything the approval's kind or the engine's capability vector cannot
/// carry. One approval surface, capability-gated decisions (decision 0048).
pub(super) fn resolve_decision_request(
    approval: &Approval,
    caps: &tidebreak_core::HarnessCaps,
    request: ApprovalDecisionRequest,
) -> Result<ApprovalDecision, ServerError> {
    use tidebreak_core::ApprovalKind as Kind;
    let structured_mismatch = |wanted: &str| {
        ServerError::unprocessable_kind(
            "approval_decision_mismatch",
            format!("this approval takes {wanted}"),
        )
    };
    match request {
        ApprovalDecisionRequest::Approve => match &approval.kind {
            // Structured kinds have structured decisions; a bare approve on
            // them would drop the payload the engine is waiting for.
            Kind::Questions { .. } => Err(structured_mismatch("answers")),
            Kind::Plan { .. } => Err(structured_mismatch("a plan decision")),
            _ => Ok(ApprovalDecision::Approve),
        },
        // Denying is always expressible: it is the fail-closed path.
        ApprovalDecisionRequest::Deny { feedback } => Ok(ApprovalDecision::Deny { feedback }),
        ApprovalDecisionRequest::ApproveWithGrant { grant_index } => {
            if caps.standing_grants != CapLevel::Supported {
                return Err(ServerError::unprocessable_kind(
                    "standing_grants_unavailable",
                    "this engine keeps no standing grants",
                ));
            }
            let Kind::ToolUse { offered_grants, .. } = &approval.kind else {
                return Err(structured_mismatch("approve or deny"));
            };
            let scope = offered_grants
                .get(usize::try_from(grant_index).unwrap_or(usize::MAX))
                .cloned()
                .ok_or_else(|| {
                    ServerError::unprocessable_kind(
                        "grant_rung_unknown",
                        format!("this approval offered no grant rung {grant_index}"),
                    )
                })?;
            Ok(ApprovalDecision::ApproveWithGrant { scope })
        }
        ApprovalDecisionRequest::Answers { answers } => {
            if caps.user_questions != CapLevel::Supported {
                return Err(ServerError::unprocessable_kind(
                    "user_questions_unavailable",
                    "this engine takes no structured answers",
                ));
            }
            let Kind::Questions { questions } = &approval.kind else {
                return Err(structured_mismatch("approve or deny"));
            };
            let asked: std::collections::HashSet<&str> = questions
                .iter()
                .map(|question| question.id.as_str())
                .collect();
            let mut seen = std::collections::HashSet::new();
            let well_formed = answers.iter().all(|answer| {
                answer.shape_is_well_formed()
                    && asked.contains(answer.question_id.as_str())
                    && seen.insert(answer.question_id.as_str())
            });
            if !well_formed || answers.is_empty() {
                return Err(ServerError::unprocessable_kind(
                    "answers_invalid",
                    "the answers do not match the questions this approval asked",
                ));
            }
            Ok(ApprovalDecision::Answers { answers })
        }
        ApprovalDecisionRequest::PlanDecision { approve, feedback } => {
            let Kind::Plan { .. } = &approval.kind else {
                return Err(structured_mismatch("approve or deny"));
            };
            Ok(ApprovalDecision::PlanDecision { approve, feedback })
        }
    }
}

impl CodeRuntime {
    pub(super) fn approval_channel(
        &self,
        owner: &OwnerId,
        session_id: SessionId,
        spawn_epoch: i64,
        mode: PermissionMode,
    ) -> Option<ApprovalChannelSpec> {
        self.approvals.revoke_session(session_id);
        if !matches!(mode, PermissionMode::Ask | PermissionMode::Auto) {
            return None;
        }
        let base = self.loopback_base.lock().expect("loopback base").clone()?;
        let token = self.approvals.issue_token(owner, session_id, spawn_epoch);
        Some(ApprovalChannelSpec {
            mcp_endpoint_url: format!("{base}/code/mcp/approval-prompt"),
            token,
            completer: self.approvals.clone(),
        })
    }

    pub async fn list_approvals(
        &self,
        owner: &OwnerId,
        state: Option<ApprovalState>,
        session_id: Option<SessionId>,
    ) -> Result<Vec<Approval>, ServerError> {
        Ok(list_approvals(&self.db, owner, state, session_id).await?)
    }

    pub async fn get_approval(
        &self,
        owner: &OwnerId,
        id: ApprovalId,
    ) -> Result<Approval, ServerError> {
        get_approval(&self.db, owner, id)
            .await?
            .ok_or_else(|| ServerError::not_found(format!("approval {id} not found")))
    }

    pub async fn record_external_approval(
        &self,
        session_id: SessionId,
        approval_id: ApprovalId,
        approval: &HarnessApprovalRef,
        raw: &serde_json::Value,
    ) -> Result<Approval, ServerError> {
        let capability = approval.capability.as_ref().ok_or_else(|| {
            ServerError::internal("external approval is missing its server capability")
        })?;
        let handle = self.require_worker(session_id)?;
        if handle.spawn_epoch != capability.spawn_epoch {
            return Err(ServerError::conflict_kind(
                "approval_worker_replaced",
                "the worker that requested this approval is no longer attached",
            ));
        }
        handle
            .sink
            .record_external_approval(approval_id, approval, raw)
            .await
            .map_err(map_worker)
    }

    pub async fn abandon_external_approval(
        &self,
        session_id: SessionId,
        approval_id: ApprovalId,
    ) -> Result<(), ServerError> {
        let session = tidebreak_core::db::code::get_session_all_owners(&self.db, session_id)
            .await?
            .ok_or_else(|| ServerError::not_found(format!("session {session_id} not found")))?;
        let Some(approval) = get_approval(&self.db, &session.owner, approval_id).await? else {
            return Ok(());
        };
        if approval.session_id != session_id {
            return Err(ServerError::internal(format!(
                "approval {approval_id} belongs to a different session"
            )));
        }
        let Some(worker_epoch) = approval.worker_epoch else {
            return Err(ServerError::internal(format!(
                "approval {approval_id} has no worker epoch"
            )));
        };
        if let Some(settlement) = abandon_pending_approval(
            &self.db,
            &session.owner,
            approval_id,
            session_id,
            worker_epoch,
            Utc::now(),
        )
        .await?
        {
            self.bus.publish(session_id, settlement.event);
            self.refresh_approval_attention(&session.owner, session_id)
                .await;
        }
        Ok(())
    }

    pub(super) async fn refresh_approval_attention(&self, owner: &OwnerId, session_id: SessionId) {
        let Ok(Some(session)) = get_session(&self.db, owner, session_id).await else {
            return;
        };
        let Ok(next) = crate::code::attention::compute_attention(
            &self.db,
            &self.bus,
            &session,
            crate::code::attention::ComputeOpts::default(),
        )
        .await
        else {
            return;
        };
        let _ = crate::code::attention::apply_attention(
            &self.db, &self.bus, owner, session_id, next, false,
        )
        .await;
    }

    pub(super) fn native_approval_ref(
        owner: &OwnerId,
        approval: &Approval,
    ) -> Result<HarnessApprovalRef, ServerError> {
        let call_id = approval.native_call_id.clone().ok_or_else(|| {
            ServerError::internal(format!("approval {} has no native call ID", approval.id))
        })?;
        if approval
            .harness_raw
            .get("call_id")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|stored| stored != call_id)
        {
            return Err(ServerError::internal(format!(
                "approval {} has conflicting native call IDs",
                approval.id
            )));
        }
        let worker_epoch = approval.worker_epoch.ok_or_else(|| {
            ServerError::internal(format!("approval {} has no worker epoch", approval.id))
        })?;
        let (Some(token), Some(request_sha256)) = (
            approval.server_capability.clone(),
            approval.request_sha256.clone(),
        ) else {
            if approval.server_capability.is_none() && approval.request_sha256.is_none() {
                return Ok(HarnessApprovalRef::engine(call_id));
            }
            return Err(ServerError::internal(format!(
                "approval {} has an incomplete server capability",
                approval.id
            )));
        };
        Ok(HarnessApprovalRef {
            call_id,
            capability: Some(HarnessApprovalCapability {
                token,
                owner_id: owner.to_string(),
                approval_id: approval.id.to_string(),
                session_id: approval.session_id.to_string(),
                turn_id: approval.turn_id.to_string(),
                spawn_epoch: worker_epoch,
                request_sha256,
            }),
        })
    }

    pub(super) async fn abandon_claim_after_delivery_failure(
        &self,
        owner: &OwnerId,
        session_id: SessionId,
        approval_id: ApprovalId,
        worker_epoch: i64,
        claim: uuid::Uuid,
    ) -> Result<(), ServerError> {
        if let Some(settlement) = settle_approval_claim(
            &self.db,
            owner,
            ClaimedApprovalSettlement {
                approval_id,
                session_id,
                worker_epoch,
                claim,
                decision: ApprovalDecisionKind::Abandoned,
                decided_at: Utc::now(),
                actor: None,
            },
        )
        .await?
        {
            self.bus.publish(session_id, settlement.event);
            self.refresh_approval_attention(owner, session_id).await;
        }
        Ok(())
    }

    pub async fn decide_approval(
        &self,
        owner: &OwnerId,
        id: ApprovalId,
        request: ApprovalDecisionRequest,
        actor: Option<tidebreak_core::TurnActor>,
    ) -> Result<Approval, ServerError> {
        let initial = self.get_approval(owner, id).await?;
        if !initial.state.is_pending() {
            return Err(ServerError::conflict_kind(
                "approval_not_pending",
                format!(
                    "approval {id} is no longer awaiting a decision: it is {}",
                    initial.state.as_str()
                ),
            ));
        }
        if initial.decision_claim.is_some() {
            return Err(ServerError::conflict_kind(
                "approval_decision_in_progress",
                format!("approval {id} already has a decision in progress"),
            ));
        }
        let handle = self.require_worker(initial.session_id)?;
        let _decision_guard = handle.approval_decisions.clone().lock_owned().await;
        // Shutdown can remove the handle before this task acquires the gate.
        // Re-read every durable precondition after the gate is ours.
        let approval = self.get_approval(owner, id).await?;
        if !approval.state.is_pending() {
            return Err(ServerError::conflict_kind(
                "approval_not_pending",
                format!(
                    "approval {id} is no longer awaiting a decision: it is {}",
                    approval.state.as_str()
                ),
            ));
        }
        if approval.decision_claim.is_some() {
            return Err(ServerError::conflict_kind(
                "approval_decision_in_progress",
                format!("approval {id} already has a decision in progress"),
            ));
        }
        let worker_epoch = approval
            .worker_epoch
            .ok_or_else(|| ServerError::internal(format!("approval {id} has no worker epoch")))?;
        let session = self.get_session(owner, approval.session_id).await?;
        if session.lifecycle != SessionLifecycle::Running {
            return Err(ServerError::conflict_kind(
                "approval_worker_inactive",
                format!(
                    "approval {id} cannot be decided while session {} is {}",
                    session.id,
                    session.lifecycle.as_str()
                ),
            ));
        }
        if session.spawn_epoch != worker_epoch || handle.spawn_epoch != worker_epoch {
            return Err(ServerError::conflict_kind(
                "approval_worker_replaced",
                "the worker that requested this approval is no longer attached",
            ));
        }
        let adapter = self.adapter(session.harness_kind)?;
        let probe = self.probe(adapter.as_ref()).await;
        let decision = resolve_decision_request(&approval, &adapter.capabilities(&probe), request)?;
        let native_ref = Self::native_approval_ref(owner, &approval)?;
        let claim = uuid::Uuid::new_v4();
        let Some(_) = claim_approval(
            &self.db,
            owner,
            id,
            approval.session_id,
            worker_epoch,
            claim,
            Utc::now(),
        )
        .await?
        else {
            let current = self.get_approval(owner, id).await?;
            let kind = if current.state.is_pending() && current.decision_claim.is_some() {
                "approval_decision_in_progress"
            } else {
                "approval_not_pending"
            };
            return Err(ServerError::conflict_kind(
                kind,
                format!("approval {id} no longer accepts this decision"),
            ));
        };
        let (reply, rx) = oneshot::channel();
        if handle
            .commands
            .send(crate::code::session_worker::WorkerCommand::Decide {
                approval: native_ref,
                decision: Box::new(decision.clone()),
                reply,
            })
            .await
            .is_err()
        {
            self.abandon_claim_after_delivery_failure(
                owner,
                approval.session_id,
                id,
                worker_epoch,
                claim,
            )
            .await?;
            return Err(ServerError::conflict_kind(
                "approval_delivery_failed",
                "the session worker stopped before it received the decision",
            ));
        }
        let native_result = match rx.await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(WorkerError::ApprovalDeliveryUnknown(message))) => {
                return Err(ServerError::conflict_kind(
                    "approval_delivery_unknown",
                    message,
                ));
            }
            Ok(Err(error)) => Err(map_worker(error)),
            Err(_) => {
                return Err(ServerError::conflict_kind(
                    "approval_delivery_unknown",
                    "the session worker stopped before it acknowledged the decision; the approval stays claimed until recovery",
                ));
            }
        };
        if let Err(error) = native_result {
            self.abandon_claim_after_delivery_failure(
                owner,
                approval.session_id,
                id,
                worker_epoch,
                claim,
            )
            .await?;
            return Err(error);
        }
        let event_decision = tidebreak_core::ApprovalDecisionKind::from(decision.clone());
        let Some(settlement) = settle_approval_claim(
            &self.db,
            owner,
            ClaimedApprovalSettlement {
                approval_id: id,
                session_id: approval.session_id,
                worker_epoch,
                claim,
                decision: event_decision,
                decided_at: Utc::now(),
                actor,
            },
        )
        .await?
        else {
            // An engine with durable parks settles a parked continuation
            // in the store operation that resumes it — answers and a plan
            // decision complete the call atomically with the row — and
            // publishes the row itself. Its claim is spent; the row says
            // so. Anything else is a lost claim.
            let current = self.get_approval(owner, id).await?;
            if adapter.capabilities(&probe).durable_parks == CapLevel::Supported
                && !current.state.is_pending()
                && current.decision_claim.is_none()
            {
                self.refresh_approval_attention(owner, approval.session_id)
                    .await;
                return Ok(current);
            }
            return Err(ServerError::internal(format!(
                "approval {id} lost its durable decision claim after native acknowledgement"
            )));
        };
        self.bus.publish(approval.session_id, settlement.event);
        self.refresh_approval_attention(owner, approval.session_id)
            .await;
        Ok(settlement.approval)
    }
}
