//! Route handlers extracted from the parent `routes` module.

use axum::extract::State;
use axum::http::StatusCode;
use chrono::Utc;
use serde::{Deserialize, Serialize};

use tidebreak_core::{ApprovalDecision, CallId, ChatId, ProjectId, TurnId};

use crate::error::ServerError;
use crate::extract::{Json, Path, Query};
use crate::scoped_store::ScopedStore;
use crate::state::AppState;

/// Body of `POST /chats/{id}/approvals/{call_id}`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalBody {
    /// `approve` or `reject`.
    pub decision: ApprovalChoice,
    /// Optional reject reason (invalid on approve).
    #[serde(default)]
    pub reason: Option<String>,
    /// How much of an approval to remember for this chat. Absent means this
    /// call only.
    #[serde(default)]
    pub grant: Option<ApprovalGrantRung>,
}

/// How wide a standing grant the human chose, narrowest first.
///
/// The renderer names a rung; the server builds the concrete grant from the
/// arguments the call is parked on. A grant can therefore only ever describe
/// the action that was actually under review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalGrantRung {
    /// Exactly the action the card showed.
    ExactAction,
    /// A leading run of the command's argv tokens, with any arguments after
    /// it — "any `cargo test`", not just "any `cargo`".
    ///
    /// The renderer names how many tokens it was offered rather than the
    /// tokens themselves. The server derives the ladder from the parked
    /// call's own arguments and honors the length only if it appears there,
    /// so a client cannot invent a prefix the card never showed.
    CommandPrefix { tokens: usize },
    /// A leading run of a workspace write's path segments — the file itself,
    /// or the directory that holds it.
    ///
    /// Named by segment count on the same terms as [`Self::CommandPrefix`]:
    /// the concrete place comes from the parked call, never from the client.
    PathPrefix { segments: usize },
    /// Every call to this tool.
    WholeTool,
}

/// Wire form of an approval decision.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalChoice {
    Approve,
    Reject,
}

/// Bounded query for restart/reconnect approval recovery.
#[derive(Debug, Deserialize)]
pub(crate) struct PendingApprovalsQuery {
    #[serde(default = "default_pending_approvals_limit")]
    pub limit: u64,
}

fn default_pending_approvals_limit() -> u64 {
    50
}

/// Closed renderer-safe pending approval projection. Canonical arguments and
/// unknown tool names never cross this boundary; only a tool's own closed
/// preview of the action under review does. That preview may carry the call's
/// own `summary`, which the approval card does not render — consent is given to
/// an action, not to a sentence about one. See
/// `docs/decisions/0015-tool-call-narration.md`.
#[derive(Debug, Serialize, ts_rs::TS)]
pub(crate) struct PendingApprovalSnapshot {
    pub call_id: CallId,
    pub turn_id: TurnId,
    pub action: tidebreak_core::RendererToolName,
    pub approval: tidebreak_core::ToolApprovalKind,
    pub class: tidebreak_core::ApprovalClass,
    /// Absent, not null, when the tool projects no action.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub preview: Option<tidebreak_core::ToolActionPreview>,
    pub can_approve: bool,
    pub can_remember: bool,
    /// Complete standing-grant ladder for this exact call, narrowest first.
    ///
    /// Empty means only one-shot approval is available. The renderer receives
    /// the whole ladder because command policy may refuse exact and whole-tool
    /// grants as well as prefixes.
    pub grant_rungs: Vec<ApprovalGrantRung>,
    /// Where the Auto-mode judge stands, when one was engaged. Absent means
    /// no judge ever owned this card.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub auto_judge_status: Option<tidebreak_core::AutoJudgeStatus>,
}

impl PendingApprovalSnapshot {
    fn from_approval(approval: tidebreak_core::ToolApproval) -> Self {
        let kind = approval.kind;
        let grant_rungs =
            approval_grant_rungs(kind, approval.preview.as_ref(), approval.action_is_exact);
        Self {
            call_id: approval.call_id,
            turn_id: approval.turn_id,
            action: tidebreak_core::RendererToolName::from(approval.tool_name.as_str()),
            approval: kind,
            class: approval.class,
            preview: approval.preview,
            can_approve: kind.is_approvable(),
            can_remember: !grant_rungs.is_empty(),
            grant_rungs,
            auto_judge_status: approval.auto_judge_status,
        }
    }
}

/// Renderer names for the complete standing-grant ladder of one approval.
pub(crate) fn approval_grant_rungs(
    kind: tidebreak_core::ToolApprovalKind,
    action: Option<&tidebreak_core::ToolActionPreview>,
    action_is_exact: bool,
) -> Vec<ApprovalGrantRung> {
    let mut scopes = match action {
        Some(action) => tidebreak_core::GrantScope::ladder_for_action(action),
        None => vec![tidebreak_core::GrantScope::WholeTool],
    };
    // A rung appears only when granting it would mint: the kind admits only
    // the rungs that describe its own action, so a workspace edit offers its
    // place rungs and an ungrantable kind offers nothing.
    scopes.retain(|scope| kind.grantable_at(scope));
    grant_rungs_from_scopes(&scopes, action_is_exact)
}

pub(crate) fn grant_rungs_from_scopes(
    scopes: &[tidebreak_core::GrantScope],
    action_is_exact: bool,
) -> Vec<ApprovalGrantRung> {
    scopes
        .iter()
        .filter_map(|scope| match scope {
            tidebreak_core::GrantScope::ExactAction(_) if action_is_exact => {
                Some(ApprovalGrantRung::ExactAction)
            }
            tidebreak_core::GrantScope::ExactAction(_) => None,
            tidebreak_core::GrantScope::CommandPrefix { tokens } => {
                Some(ApprovalGrantRung::CommandPrefix {
                    tokens: tokens.len(),
                })
            }
            tidebreak_core::GrantScope::PathSubtree { prefix } => {
                Some(ApprovalGrantRung::PathPrefix {
                    segments: prefix.split('/').count(),
                })
            }
            tidebreak_core::GrantScope::WholeTool => Some(ApprovalGrantRung::WholeTool),
            // Retained for old durable grants; the current ladder names the
            // same authority as a one-token command prefix.
            tidebreak_core::GrantScope::AnyArgsFor { .. } => {
                Some(ApprovalGrantRung::CommandPrefix { tokens: 1 })
            }
        })
        .collect()
}

/// `GET /chats/{id}/approvals` — recover a bounded page of pending cards.
pub(crate) async fn list_pending_approvals(
    store: ScopedStore,
    Path(chat_id): Path<ChatId>,
    Query(query): Query<PendingApprovalsQuery>,
) -> Result<Json<Vec<PendingApprovalSnapshot>>, ServerError> {
    if !(1..=100).contains(&query.limit) {
        return Err(ServerError::bad_request(
            "approval limit must be between 1 and 100",
        ));
    }
    store.require_chat(chat_id).await?;
    let approvals = store
        .list_pending_tool_call_approvals(chat_id, query.limit)
        .await?;
    Ok(Json(
        approvals
            .into_iter()
            .map(PendingApprovalSnapshot::from_approval)
            .collect(),
    ))
}

/// One durable "don't ask again" the reader has made, with enough provenance
/// to recognize it later and withdraw it. Grant scopes are already closed
/// renderer-safe projections, so the snapshot carries them verbatim.
#[derive(Debug, Serialize, ts_rs::TS)]
pub(crate) struct StandingGrantSnapshot {
    /// The approval decision that created the grant — also the handle a
    /// revocation names.
    pub source_call_id: CallId,
    /// How far the grant reaches — one chat, or every chat in a project.
    pub level: tidebreak_core::GrantLevel,
    /// The name of whatever the level points at, for provenance. `None` when
    /// that chat or project is untitled.
    pub level_title: Option<String>,
    pub action: tidebreak_core::RendererToolName,
    pub approval: tidebreak_core::ToolApprovalKind,
    pub scope: tidebreak_core::GrantScope,
    pub granted_at: chrono::DateTime<Utc>,
}

/// `GET /grants` — the principal's standing grants, newest first, across all
/// of their chats.
///
/// The settings surface for "what the agent can do without asking": a grant
/// the reader cannot find is a one-way door, and this is where it is found.
/// Grants are owner-scoped through the chat or project their level points at
/// (#853), and the provenance titles resolve through the same principal's
/// chats and projects.
pub(crate) async fn list_standing_grants(
    store: ScopedStore,
) -> Result<Json<Vec<StandingGrantSnapshot>>, ServerError> {
    standing_grant_snapshots(&store).await.map(Json)
}

async fn standing_grant_snapshots(
    store: &ScopedStore,
) -> Result<Vec<StandingGrantSnapshot>, ServerError> {
    let grants = store.list_standing_tool_grants().await?;
    let chat_titles: std::collections::HashMap<ChatId, Option<String>> = store
        .list_chats()
        .await?
        .into_iter()
        .map(|chat| (chat.id, chat.title))
        .collect();
    let project_titles: std::collections::HashMap<ProjectId, Option<String>> = store
        .list_projects()
        .await?
        .into_iter()
        .map(|project| (project.id, project.title))
        .collect();
    Ok(grants
        .into_iter()
        .map(|record| {
            let level = record.grant.level();
            let level_title = match level {
                tidebreak_core::GrantLevel::Chat { chat_id } => {
                    chat_titles.get(&chat_id).cloned().flatten()
                }
                tidebreak_core::GrantLevel::Project { project_id } => {
                    project_titles.get(&project_id).cloned().flatten()
                }
            };
            StandingGrantSnapshot {
                source_call_id: record.source_call_id,
                level,
                level_title,
                action: tidebreak_core::RendererToolName::from(record.grant.tool_name()),
                approval: record.grant.kind(),
                scope: record.grant.scope().clone(),
                granted_at: record.grant.granted_at(),
            }
        })
        .collect())
}

/// `GET /consent/statements` — the server's rows of the unified consent read
/// model: every standing tool grant as one [`ConsentStatementSnapshot`].
///
/// The capability half of the union lives in the desktop's host broker and
/// joins these rows renderer-side; the server serves what its own store
/// holds, in the shared statement shape, so both halves render as one list.
pub(crate) async fn list_consent_statements(
    store: ScopedStore,
) -> Result<Json<Vec<crate::consent::ConsentStatementSnapshot>>, ServerError> {
    Ok(Json(
        standing_grant_snapshots(&store)
            .await?
            .into_iter()
            .map(|grant| crate::consent::ConsentStatementSnapshot {
                handle: crate::consent::ConsentHandle::ToolGrant {
                    call_id: grant.source_call_id,
                },
                level: grant.level,
                level_title: grant.level_title,
                verb: crate::consent::ConsentVerb::Tool {
                    action: grant.action,
                    approval: grant.approval,
                },
                resource: crate::consent::ConsentResource::ActionScope { scope: grant.scope },
                method: crate::consent::ConsentMethodSnapshot::ApprovalCard,
                granted_at: grant.granted_at,
            })
            .collect(),
    ))
}

/// `DELETE /grants/{call_id}` — withdraw a standing grant. Later matching
/// calls park on the approval card again. `204` on success, `404` when the
/// grant does not exist (already revoked, or never granted).
pub(crate) async fn delete_standing_grant(
    store: ScopedStore,
    Path(call_id): Path<CallId>,
) -> Result<StatusCode, ServerError> {
    if store.revoke_standing_tool_grant(call_id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ServerError::not_found(format!(
            "standing grant {call_id} not found"
        )))
    }
}

/// `POST /chats/{id}/approvals/{call_id}` — decide a parked Sensitive tool call.
///
/// `204` on success. `404` if the chat or call isn't pending. The turn stays
/// holding its slot until it finishes after the decision.
pub async fn post_approval(
    State(state): State<AppState>,
    store: ScopedStore,
    Path((chat_id, call_id)): Path<(ChatId, CallId)>,
    Json(body): Json<ApprovalBody>,
) -> Result<StatusCode, ServerError> {
    // Confirm the chat exists so a typo'd id doesn't look like "not pending".
    store.require_chat(chat_id).await?;
    let decision = match body.decision {
        ApprovalChoice::Approve => {
            if body.reason.is_some() {
                return Err(ServerError::bad_request(
                    "approval reason is only valid when rejecting",
                ));
            }
            ApprovalDecision::Approve
        }
        ApprovalChoice::Reject => ApprovalDecision::Reject {
            reason: body
                .reason
                .map(|reason| reason.trim().to_owned())
                .filter(|reason| !reason.is_empty())
                .unwrap_or_else(|| tidebreak_core::ToolApproval::DEFAULT_REJECT_REASON.into()),
        },
    };
    if body.grant.is_some() && !matches!(&decision, ApprovalDecision::Approve) {
        return Err(ServerError::bad_request(
            "only an approval can be remembered",
        ));
    }
    if decision
        .reason()
        .is_some_and(|reason| !tidebreak_core::ToolApproval::valid_reason(reason))
    {
        return Err(ServerError::bad_request(
            "approval reject reason is invalid",
        ));
    }
    match state
        .approvals
        .resolve_with_grant(chat_id, call_id, decision, body.grant)
        .await?
    {
        crate::approvals::ResolveApprovalOutcome::Resolved => Ok(StatusCode::NO_CONTENT),
        crate::approvals::ResolveApprovalOutcome::NotPending => Err(ServerError::not_found(
            format!("no pending approval for call {call_id}"),
        )),
        crate::approvals::ResolveApprovalOutcome::WrongChat => Err(ServerError::not_found(
            format!("no pending approval for call {call_id}"),
        )),
        crate::approvals::ResolveApprovalOutcome::NotApprovable => Err(ServerError::conflict_kind(
            "approval_action_not_presentable",
            "this action cannot be approved from the renderer",
        )),
        // Refusing beats widening: a rung the parked call cannot describe
        // would otherwise have to fall back to a broader grant than the human
        // was shown.
        crate::approvals::ResolveApprovalOutcome::GrantNotAvailable => Err(
            ServerError::bad_request("this action cannot be granted at that scope"),
        ),
        crate::approvals::ResolveApprovalOutcome::DecisionConflict => {
            Err(ServerError::conflict_kind(
                "approval_already_decided",
                "this approval was already decided differently",
            ))
        }
    }
}
