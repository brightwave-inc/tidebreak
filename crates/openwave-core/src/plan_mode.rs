//! Bounded plan proposals and exact user decisions.
//!
//! A plan is a continuation, not a chat message: `exit_plan_mode` parks the
//! foreground turn the way `ask_user_questions` does, and the user's decision
//! completes the same tool call and resumes the blocked turn. Accepting is the
//! one place a permission mode changes as a side effect — the chat leaves
//! [`crate::PermissionMode::Plan`] for the execution mode the decision names,
//! so the resumed turn re-freezes its surface with execution tools.

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{CallId, ChatId, PermissionMode, ToolSpec, TurnId};

/// Stable foreground-only tool name, advertised only to plan-mode turns.
pub const EXIT_PLAN_MODE_TOOL: &str = "exit_plan_mode";

pub const MAX_PLAN_TITLE_CHARS: usize = 120;
pub const MIN_PLAN_CONTENT_CHARS: usize = 50;
pub const MAX_PLAN_CONTENT_CHARS: usize = 40_000;
pub const MAX_PLAN_FEEDBACK_CHARS: usize = 4_000;

/// Canonical model arguments for [`EXIT_PLAN_MODE_TOOL`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExitPlanModeArgs {
    /// A concise title naming what the plan will do.
    #[schemars(length(min = 1, max = MAX_PLAN_TITLE_CHARS))]
    pub title: String,
    /// The complete plan in well-structured Markdown: the intended steps,
    /// what each one touches, and any decisions already settled.
    #[schemars(length(min = MIN_PLAN_CONTENT_CHARS, max = MAX_PLAN_CONTENT_CHARS))]
    pub plan: String,
}

impl ExitPlanModeArgs {
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        valid_single_line(&self.title, MAX_PLAN_TITLE_CHARS)
            && valid_markdown(&self.plan, MIN_PLAN_CONTENT_CHARS, MAX_PLAN_CONTENT_CHARS)
    }
}

/// Renderer-safe, durable card projection of a proposed plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
pub struct PendingPlanApproval {
    pub call_id: CallId,
    pub turn_id: TurnId,
    pub title: String,
    pub plan: String,
    pub proposed_at: DateTime<Utc>,
}

/// The two decisions a reader can make about a proposed plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum PlanDecisionChoice {
    Accept,
    Reject,
}

/// Exact decision command for a pending plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanDecision {
    pub decision: PlanDecisionChoice,
    /// Reader guidance fed back to the model. Meaningful on reject; ignored
    /// content-wise on accept but validated the same way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feedback: Option<String>,
    /// The mode the chat continues in after an accept. `None` means
    /// [`PermissionMode::Auto`]; [`PermissionMode::Plan`] is invalid — the
    /// point of accepting is to leave it. Must be absent on reject.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<PermissionMode>,
}

/// The mode an accepted plan hands the chat to when the decision names none.
pub const DEFAULT_ACCEPTED_PLAN_MODE: PermissionMode = PermissionMode::Auto;

impl PlanDecision {
    #[must_use]
    pub fn shape_is_well_formed(&self) -> bool {
        let feedback_valid = self
            .feedback
            .as_deref()
            .is_none_or(|feedback| valid_markdown(feedback, 1, MAX_PLAN_FEEDBACK_CHARS));
        let mode_valid = match self.decision {
            PlanDecisionChoice::Accept => self.permission_mode != Some(PermissionMode::Plan),
            PlanDecisionChoice::Reject => self.permission_mode.is_none(),
        };
        feedback_valid && mode_valid
    }

    /// The mode the chat runs in after this decision commits.
    #[must_use]
    pub fn mode_after(&self) -> Option<PermissionMode> {
        match self.decision {
            PlanDecisionChoice::Accept => {
                Some(self.permission_mode.unwrap_or(DEFAULT_ACCEPTED_PLAN_MODE))
            }
            PlanDecisionChoice::Reject => None,
        }
    }
}

/// Exact storage command with its conversation scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecidePlanRequest {
    pub chat_id: ChatId,
    pub call_id: CallId,
    pub decision: PlanDecision,
}

/// Persisted lifecycle of a plan continuation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanRequestStatus {
    Pending,
    Accepted,
    Rejected,
    Cancelled,
}

impl PlanRequestStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
            Self::Cancelled => "cancelled",
        }
    }
}

/// The model-facing result committed onto the completed tool call.
///
/// This is what the resumed turn reads where a tool result goes, so the note
/// carries the whole contract: an accepted plan must start executing without
/// another confirmation, and a rejected one must be revised in plan mode.
#[must_use]
pub fn plan_decision_result(decision: &PlanDecision) -> Value {
    match decision.decision {
        PlanDecisionChoice::Accept => {
            let mode = decision
                .permission_mode
                .unwrap_or(DEFAULT_ACCEPTED_PLAN_MODE);
            serde_json::json!({
                "decision": "accepted",
                "permission_mode": mode.as_str(),
                "note": format!(
                    "The user accepted the plan. This chat has left plan mode and now runs in {} mode with its full tool surface. Begin executing the plan immediately; do not wait for further confirmation.",
                    mode.as_str()
                ),
            })
        }
        PlanDecisionChoice::Reject => serde_json::json!({
            "decision": "rejected",
            "feedback": decision.feedback,
            "note": "The user sent the plan back. The chat remains in plan mode: revise the plan using the feedback and submit it again with exit_plan_mode.",
        }),
    }
}

/// Validate canonical model arguments before checkpointing.
#[must_use]
pub fn validate_exit_plan_mode_arguments(arguments: &Value) -> bool {
    serde_json::from_value::<ExitPlanModeArgs>(arguments.clone())
        .is_ok_and(|arguments| arguments.is_well_formed())
}

#[must_use]
pub fn exit_plan_mode_tool_spec() -> ToolSpec {
    ToolSpec::for_args::<ExitPlanModeArgs>(
        EXIT_PLAN_MODE_TOOL,
        "Pause the current plan-mode turn and present the finished plan for the user's decision. Call it once your exploration is complete and the plan is concrete, with the full plan in Markdown. Call this tool alone, with no assistant text or sibling tools. If the user accepts, the chat leaves plan mode and you execute the plan; if they send it back, revise using their feedback and submit again.",
    )
}

fn valid_single_line(value: &str, max_chars: usize) -> bool {
    !value.trim().is_empty()
        && value.chars().count() <= max_chars
        && !value.chars().any(char::is_control)
}

fn valid_markdown(value: &str, min_chars: usize, max_chars: usize) -> bool {
    let chars = value.chars().count();
    !value.trim().is_empty()
        && chars >= min_chars
        && chars <= max_chars
        && !value
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\t'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Value {
        serde_json::json!({
            "title": "Add health checks",
            "plan": "## Steps\n1. Add a `/healthz` route to the server.\n2. Cover it with one lifecycle test.\n",
        })
    }

    #[test]
    fn contract_is_closed_and_bounded() {
        assert!(validate_exit_plan_mode_arguments(&sample()));
        let mut short = sample();
        short["plan"] = Value::String("too short".into());
        assert!(!validate_exit_plan_mode_arguments(&short));
        let mut unknown = sample();
        unknown["extra"] = Value::String("no".into());
        assert!(!validate_exit_plan_mode_arguments(&unknown));
        let mut control = sample();
        control["title"] = Value::String("line\nbreak".into());
        assert!(!validate_exit_plan_mode_arguments(&control));
    }

    #[test]
    fn decisions_gate_the_mode_hand_off() {
        let accept = PlanDecision {
            decision: PlanDecisionChoice::Accept,
            feedback: None,
            permission_mode: None,
        };
        assert!(accept.shape_is_well_formed());
        assert_eq!(accept.mode_after(), Some(PermissionMode::Auto));
        assert!(!PlanDecision {
            permission_mode: Some(PermissionMode::Plan),
            ..accept.clone()
        }
        .shape_is_well_formed());
        let reject = PlanDecision {
            decision: PlanDecisionChoice::Reject,
            feedback: Some("Split step 2 into its own slice.".into()),
            permission_mode: None,
        };
        assert!(reject.shape_is_well_formed());
        assert_eq!(reject.mode_after(), None);
        assert!(!PlanDecision {
            permission_mode: Some(PermissionMode::Ask),
            ..reject.clone()
        }
        .shape_is_well_formed());
    }

    #[test]
    fn accepted_result_names_the_mode_and_orders_execution() {
        let result = plan_decision_result(&PlanDecision {
            decision: PlanDecisionChoice::Accept,
            feedback: None,
            permission_mode: Some(PermissionMode::Ask),
        });
        assert_eq!(result["decision"], "accepted");
        assert_eq!(result["permission_mode"], "ask");
        assert!(result["note"].as_str().unwrap().contains("left plan mode"));
    }
}
