//! The print-mode driving protocol: the events an unattended run emits when it
//! needs an answer, and the decision lines a driving process writes back.
//!
//! This is a wire contract. Both directions are one JSON object per line;
//! emitted objects carry `"tidebreak": "v1"` so a consumer can tell a CLI
//! control event apart from a journal frame (which carries `seq`/`event`) on
//! the same stream, and so the protocol can be versioned without guessing.
//!
//! Unknown fields on a decision line are ignored — a newer driver may send
//! more than this build reads. Unknown *variants* are not: an unrecognized
//! `type` or verdict is reported back as an `error` event rather than
//! silently doing something the driver did not ask for.

use serde::{Deserialize, Serialize};
use tidebreak_core::CallId;

use crate::api::wire::{GrantRung, PendingPlan, PendingQuestions};

/// Version tag stamped on every event this module emits.
pub const PROTOCOL_VERSION: &str = "v1";

/// Something the turn parked on, waiting for an answer.
#[derive(Debug, Clone, PartialEq)]
pub enum Interaction {
    Approval {
        call_id: CallId,
        action: String,
        approval: String,
        grant_rungs: Vec<GrantRung>,
        preview: Option<serde_json::Value>,
    },
    Plan {
        call_id: CallId,
        title: String,
        plan: String,
    },
    Questions {
        call_id: CallId,
        questions: Vec<QuestionPrompt>,
    },
}

/// One question as the driver sees it: enough to answer without reading the
/// journal.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QuestionPrompt {
    pub id: String,
    pub question: String,
    pub header: String,
    pub question_type: String,
    pub allow_free_form: bool,
    pub options: Vec<QuestionOptionPrompt>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QuestionOptionPrompt {
    pub id: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl Interaction {
    pub fn from_plan(plan: PendingPlan) -> Self {
        Self::Plan {
            call_id: plan.call_id,
            title: plan.title,
            plan: plan.plan,
        }
    }

    pub fn from_questions(pending: PendingQuestions) -> Self {
        Self::Questions {
            call_id: pending.call_id,
            questions: pending
                .questions
                .into_iter()
                .map(|question| QuestionPrompt {
                    id: question.id,
                    question: question.question,
                    header: question.header,
                    question_type: question.question_type,
                    allow_free_form: question.allow_free_form,
                    options: question
                        .options
                        .into_iter()
                        .map(|option| QuestionOptionPrompt {
                            id: option.id,
                            label: option.label,
                            description: option.description,
                        })
                        .collect(),
                })
                .collect(),
        }
    }

    pub fn call_id(&self) -> CallId {
        match self {
            Self::Approval { call_id, .. }
            | Self::Plan { call_id, .. }
            | Self::Questions { call_id, .. } => *call_id,
        }
    }

    /// The `type` of both the request event and the decision line that answers
    /// it.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Approval { .. } => "approval",
            Self::Plan { .. } => "plan",
            Self::Questions { .. } => "questions",
        }
    }

    /// The event emitted to ask the driver for a decision.
    pub fn request_event(&self) -> serde_json::Value {
        let mut event = match self {
            Self::Approval {
                call_id,
                action,
                approval,
                grant_rungs,
                preview,
            } => serde_json::json!({
                "type": "approval_request",
                "call_id": call_id,
                "action": action,
                "approval": approval,
                "grant_rungs": grant_rungs,
                "preview": preview,
            }),
            Self::Plan {
                call_id,
                title,
                plan,
            } => serde_json::json!({
                "type": "plan_proposal",
                "call_id": call_id,
                "title": title,
                "plan": plan,
            }),
            Self::Questions { call_id, questions } => serde_json::json!({
                "type": "questions_asked",
                "call_id": call_id,
                "questions": questions,
            }),
        };
        event["tidebreak"] = serde_json::json!(PROTOCOL_VERSION);
        event
    }

    /// What happens when no driver answers.
    pub fn undriven(&self) -> Undriven {
        match self {
            Self::Approval { call_id, .. } => Undriven::Halt(Halt {
                reason: HaltReason::ApprovalDriverUnavailable,
                call_id: Some(*call_id),
                message: "the turn requested approval and no driver is attached to decide it"
                    .to_owned(),
            }),
            Self::Plan { call_id, .. } => Undriven::Halt(Halt {
                reason: HaltReason::PlanUndriven,
                call_id: Some(*call_id),
                message: "the turn proposed a plan and no driver is attached to decide it"
                    .to_owned(),
            }),
            Self::Questions { call_id, .. } => Undriven::Halt(Halt {
                reason: HaltReason::QuestionsUndriven,
                call_id: Some(*call_id),
                message:
                    "the turn asked the user a question and no driver is attached to answer it"
                        .to_owned(),
            }),
        }
    }
}

/// The outcome for an interaction nobody is driving.
#[derive(Debug, Clone, PartialEq)]
pub enum Undriven {
    /// The turn ends because no caller can answer the interaction.
    Halt(Halt),
}

/// A decision to carry out against the server.
#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    Approval {
        approve: bool,
        reason: String,
        grant: Option<GrantRung>,
    },
    Plan {
        accept: bool,
        feedback: Option<String>,
        permission_mode: Option<String>,
    },
    Questions {
        /// The `{ "answers": [...] }` body the answer route takes.
        body: serde_json::Value,
    },
}

/// Why an unattended run gave up, in a form a caller can branch on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HaltReason {
    ApprovalDriverUnavailable,
    PlanUndriven,
    QuestionsUndriven,
    DecisionFailed,
    /// The request behind a parked interaction could not be read at all, so the
    /// run cannot tell what it is waiting on.
    PendingLookupFailed,
    /// A parked `request_folder_access` call could not be refused. Its own
    /// reason, not `decision_failed`: nothing a driver said produced it, and a
    /// caller reading exit 4 should be able to tell that a folder request —
    /// which no decision line can ever answer — is what ended the run.
    FolderDeclineFailed,
    /// SIGINT reached the run; the turn is cancelled rather than abandoned.
    Interrupted,
}

impl HaltReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ApprovalDriverUnavailable => "approval_driver_unavailable",
            Self::PlanUndriven => "plan_undriven",
            Self::QuestionsUndriven => "questions_undriven",
            Self::DecisionFailed => "decision_failed",
            Self::PendingLookupFailed => "pending_lookup_failed",
            Self::FolderDeclineFailed => "folder_decline_failed",
            Self::Interrupted => "interrupted",
        }
    }

    /// The process exit status this halt produces. The undriven reasons share
    /// one code — they are the same fact ("nobody was there to answer") and the
    /// `reason` field separates them. The three failure reasons share one for
    /// the same reason: the run reached something it had to settle and could
    /// not carry it through.
    pub fn exit_code(self) -> i32 {
        match self {
            Self::ApprovalDriverUnavailable | Self::PlanUndriven | Self::QuestionsUndriven => {
                super::EXIT_INTERACTION_UNDRIVEN
            }
            Self::DecisionFailed | Self::PendingLookupFailed | Self::FolderDeclineFailed => {
                super::EXIT_DECISION_FAILED
            }
            Self::Interrupted => super::EXIT_INTERRUPTED,
        }
    }
}

/// A terminating outcome plus the machine-readable reason that explains it.
#[derive(Debug, Clone, PartialEq)]
pub struct Halt {
    pub reason: HaltReason,
    pub call_id: Option<CallId>,
    pub message: String,
}

impl Halt {
    pub fn event(&self) -> serde_json::Value {
        serde_json::json!({
            "tidebreak": PROTOCOL_VERSION,
            "type": "halted",
            "reason": self.reason.as_str(),
            "exit_code": self.reason.exit_code(),
            "call_id": self.call_id,
            "message": self.message,
        })
    }
}

/// A driver-facing complaint about a decision line. Emitted, then the next
/// line is read: a malformed line never decides anything by default.
pub fn error_event(message: &str) -> serde_json::Value {
    serde_json::json!({
        "tidebreak": PROTOCOL_VERSION,
        "type": "error",
        "message": message,
    })
}

/// One decision line, as written on stdin.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum DecisionLine {
    Approval {
        #[serde(default)]
        call_id: Option<CallId>,
        decision: ApprovalVerdict,
        #[serde(default)]
        reason: Option<String>,
        #[serde(default)]
        grant: Option<GrantRung>,
    },
    Plan {
        #[serde(default)]
        call_id: Option<CallId>,
        decision: PlanVerdict,
        #[serde(default)]
        feedback: Option<String>,
        #[serde(default)]
        permission_mode: Option<String>,
    },
    Questions {
        #[serde(default)]
        call_id: Option<CallId>,
        answers: Vec<QuestionAnswer>,
    },
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ApprovalVerdict {
    Approve,
    Reject,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PlanVerdict {
    Accept,
    Reject,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct QuestionAnswer {
    question_id: String,
    #[serde(default)]
    selected_option_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    custom_answer: Option<String>,
}

impl DecisionLine {
    fn kind(&self) -> &'static str {
        match self {
            Self::Approval { .. } => "approval",
            Self::Plan { .. } => "plan",
            Self::Questions { .. } => "questions",
        }
    }

    fn call_id(&self) -> Option<CallId> {
        match self {
            Self::Approval { call_id, .. }
            | Self::Plan { call_id, .. }
            | Self::Questions { call_id, .. } => *call_id,
        }
    }
}

/// Read one decision line as the answer to `interaction`.
///
/// `Err` is a message for the driver: the line is rejected, not applied
/// approximately. A line may omit `call_id`, which answers whatever is parked;
/// naming a different call is an error rather than a redirect, since only one
/// interaction is ever outstanding here.
pub fn parse_decision(
    line: &str,
    interaction: &Interaction,
) -> std::result::Result<Decision, String> {
    let parsed: DecisionLine = serde_json::from_str(line)
        .map_err(|error| format!("could not read the decision line: {error}"))?;
    if parsed.kind() != interaction.kind() {
        return Err(format!(
            "a {} decision does not answer the pending {} request",
            parsed.kind(),
            interaction.kind()
        ));
    }
    if let Some(named) = parsed.call_id() {
        if named != interaction.call_id() {
            return Err(format!(
                "decision names call {named}, but call {} is pending",
                interaction.call_id()
            ));
        }
    }
    Ok(match parsed {
        DecisionLine::Approval {
            decision,
            reason,
            grant,
            ..
        } => Decision::Approval {
            approve: matches!(decision, ApprovalVerdict::Approve),
            reason: reason.unwrap_or_else(|| "rejected by the driver".to_owned()),
            grant,
        },
        DecisionLine::Plan {
            decision,
            feedback,
            permission_mode,
            ..
        } => Decision::Plan {
            accept: matches!(decision, PlanVerdict::Accept),
            feedback,
            permission_mode,
        },
        DecisionLine::Questions { answers, .. } => Decision::Questions {
            body: serde_json::json!({ "answers": answers }),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(call_id: CallId) -> Interaction {
        Interaction::Plan {
            call_id,
            title: "Ship it".into(),
            plan: "1. do the thing".into(),
        }
    }

    /// The decision line's own words survive into the decision: a build that
    /// answered from policy instead would lose the feedback and the mode.
    #[test]
    fn a_plan_decision_line_carries_the_drivers_own_words() {
        let call_id = CallId::new();
        let decision = parse_decision(
            &format!(
                r#"{{"type":"plan","call_id":"{call_id}","decision":"accept","feedback":"go","permission_mode":"allow"}}"#
            ),
            &plan(call_id),
        )
        .expect("the line answers the pending plan");
        assert_eq!(
            decision,
            Decision::Plan {
                accept: true,
                feedback: Some("go".into()),
                permission_mode: Some("allow".into()),
            }
        );
    }

    /// A line that answers something else is refused rather than applied to
    /// whatever happens to be parked.
    #[test]
    fn mismatched_decision_lines_are_refused() {
        let call_id = CallId::new();
        let wrong_kind = parse_decision(
            r#"{"type":"approval","decision":"approve"}"#,
            &plan(call_id),
        );
        assert!(
            wrong_kind
                .as_ref()
                .is_err_and(|message| message.contains("does not answer")),
            "{wrong_kind:?}"
        );

        let other_call = CallId::new();
        let wrong_call = parse_decision(
            &format!(r#"{{"type":"plan","call_id":"{other_call}","decision":"accept"}}"#),
            &plan(call_id),
        );
        assert!(
            wrong_call
                .as_ref()
                .is_err_and(|message| message.contains("is pending")),
            "{wrong_call:?}"
        );
    }

    /// Host folder access is not something a driver may grant. The protocol's
    /// vocabulary is closed at approval/plan/questions, so no stdin line can
    /// resolve a `request_folder_access` call however it is spelled — a driven
    /// run refuses folders exactly like an undriven one, and standing consent
    /// comes only from `tidebreak folder connect`.
    ///
    /// If a folder verb is ever added here, this fails first and on purpose:
    /// it is the guard on that decision, not an accident of the parser.
    #[test]
    fn no_decision_line_can_grant_a_folder() {
        let call_id = CallId::new();
        for line in [
            r#"{"type":"folder","decision":"allow"}"#,
            r#"{"type":"folder_access","decision":"approve","path":"/srv/data"}"#,
            r#"{"type":"request_folder_access","decision":"approve"}"#,
            // An approval-shaped line must not be repurposed either: it can only
            // ever answer a pending approval, and a folder request never is one.
            r#"{"type":"approval","decision":"approve","path":"/srv/data"}"#,
        ] {
            let parsed = parse_decision(line, &plan(call_id));
            assert!(
                parsed.is_err(),
                "a driver line was accepted as a folder decision: {line}"
            );
        }
    }
}
