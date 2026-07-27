//! Bounded foreground-agent questions and exact user answers.
//!
//! These contracts deliberately separate the canonical model proposal from the
//! renderer-facing durable projection. A question is a continuation, not a new
//! chat message: answering it completes the same tool call and resumes the
//! blocked turn.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{CallId, ChatId, ToolSpec, TurnId};

/// Stable foreground-only tool name.
pub const ASK_USER_QUESTIONS_TOOL: &str = "ask_user_questions";

pub const MAX_USER_QUESTIONS: usize = 3;
pub const MAX_QUESTION_ID_CHARS: usize = 64;
pub const MAX_QUESTION_HEADER_CHARS: usize = 32;
pub const MAX_QUESTION_PROMPT_CHARS: usize = 500;
pub const MAX_QUESTION_OPTIONS: usize = 5;
pub const MAX_QUESTION_OPTION_ID_CHARS: usize = 64;
pub const MAX_QUESTION_OPTION_LABEL_CHARS: usize = 80;
pub const MAX_QUESTION_OPTION_DESCRIPTION_CHARS: usize = 240;
pub const MAX_FREE_FORM_ANSWER_CHARS: usize = 2_000;

/// One mutually exclusive answer choice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct UserQuestionOption {
    pub id: String,
    pub label: String,
    pub description: String,
}

impl UserQuestionOption {
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        valid_text(&self.id, MAX_QUESTION_OPTION_ID_CHARS)
            && valid_text(&self.label, MAX_QUESTION_OPTION_LABEL_CHARS)
            && valid_text(&self.description, MAX_QUESTION_OPTION_DESCRIPTION_CHARS)
    }
}

/// One bounded question shown to the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct UserQuestion {
    pub id: String,
    pub header: String,
    pub question: String,
    #[serde(default)]
    pub options: Vec<UserQuestionOption>,
    #[serde(default)]
    pub allow_free_form: bool,
}

impl UserQuestion {
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        if !valid_text(&self.id, MAX_QUESTION_ID_CHARS)
            || !valid_text(&self.header, MAX_QUESTION_HEADER_CHARS)
            || !valid_text(&self.question, MAX_QUESTION_PROMPT_CHARS)
            || self.options.len() > MAX_QUESTION_OPTIONS
            || (self.options.is_empty() && !self.allow_free_form)
        {
            return false;
        }
        let mut option_ids = HashSet::with_capacity(self.options.len());
        self.options
            .iter()
            .all(|option| option.is_well_formed() && option_ids.insert(option.id.as_str()))
    }
}

/// Canonical model arguments for [`ASK_USER_QUESTIONS_TOOL`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AskUserQuestionsArgs {
    pub questions: Vec<UserQuestion>,
}

impl AskUserQuestionsArgs {
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        if self.questions.is_empty() || self.questions.len() > MAX_USER_QUESTIONS {
            return false;
        }
        let mut ids = HashSet::with_capacity(self.questions.len());
        self.questions
            .iter()
            .all(|question| question.is_well_formed() && ids.insert(question.id.as_str()))
    }
}

/// One exact answer. Exactly one of `option_id` and `free_form` is populated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserQuestionAnswer {
    pub question_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub option_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub free_form: Option<String>,
}

impl UserQuestionAnswer {
    #[must_use]
    pub fn shape_is_well_formed(&self) -> bool {
        valid_text(&self.question_id, MAX_QUESTION_ID_CHARS)
            && match (&self.option_id, &self.free_form) {
                (Some(option), None) => valid_text(option, MAX_QUESTION_OPTION_ID_CHARS),
                (None, Some(answer)) => valid_free_form(answer),
                _ => false,
            }
    }
}

/// Exact answer command for a pending request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnswerUserQuestions {
    pub answers: Vec<UserQuestionAnswer>,
}

impl AnswerUserQuestions {
    #[must_use]
    pub fn shape_is_well_formed(&self) -> bool {
        if self.answers.is_empty() || self.answers.len() > MAX_USER_QUESTIONS {
            return false;
        }
        let mut ids = HashSet::with_capacity(self.answers.len());
        self.answers
            .iter()
            .all(|answer| answer.shape_is_well_formed() && ids.insert(answer.question_id.as_str()))
    }
}

/// Renderer-safe, durable card projection.
///
/// It contains only the validated presentation contract. Provider metadata,
/// raw tool arguments, leases, executor identities, and diagnostics stay
/// behind the server boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
pub struct PendingUserQuestions {
    pub call_id: CallId,
    pub turn_id: TurnId,
    pub questions: Vec<UserQuestion>,
    pub asked_at: DateTime<Utc>,
}

/// Persisted lifecycle of a question continuation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserQuestionRequestStatus {
    Pending,
    Answered,
    Cancelled,
}

impl UserQuestionRequestStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Answered => "answered",
            Self::Cancelled => "cancelled",
        }
    }
}

/// Exact storage command with its conversation scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnswerUserQuestionsRequest {
    pub chat_id: ChatId,
    pub call_id: CallId,
    pub answers: AnswerUserQuestions,
}

/// Validate canonical model arguments before checkpointing.
#[must_use]
pub fn validate_ask_user_questions_arguments(arguments: &Value) -> bool {
    serde_json::from_value::<AskUserQuestionsArgs>(arguments.clone())
        .is_ok_and(|arguments| arguments.is_well_formed())
}

#[must_use]
pub fn ask_user_questions_tool_spec() -> ToolSpec {
    ToolSpec {
        name: ASK_USER_QUESTIONS_TOOL.into(),
        description: "Pause the current foreground turn and ask the user up to three short structured questions. Use stable question and option IDs. Options are mutually exclusive; enable allow_free_form only when a custom answer is useful. Call this tool alone, with no assistant text or sibling tools.".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "questions": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": MAX_USER_QUESTIONS,
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": { "type": "string", "minLength": 1, "maxLength": MAX_QUESTION_ID_CHARS },
                            "header": { "type": "string", "minLength": 1, "maxLength": MAX_QUESTION_HEADER_CHARS },
                            "question": { "type": "string", "minLength": 1, "maxLength": MAX_QUESTION_PROMPT_CHARS },
                            "options": {
                                "type": "array",
                                "maxItems": MAX_QUESTION_OPTIONS,
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "id": { "type": "string", "minLength": 1, "maxLength": MAX_QUESTION_OPTION_ID_CHARS },
                                        "label": { "type": "string", "minLength": 1, "maxLength": MAX_QUESTION_OPTION_LABEL_CHARS },
                                        "description": { "type": "string", "minLength": 1, "maxLength": MAX_QUESTION_OPTION_DESCRIPTION_CHARS }
                                    },
                                    "required": ["id", "label", "description"],
                                    "additionalProperties": false
                                }
                            },
                            "allow_free_form": { "type": "boolean" }
                        },
                        "required": ["id", "header", "question"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["questions"],
            "additionalProperties": false
        }),
    }
}

fn valid_text(value: &str, max_chars: usize) -> bool {
    !value.trim().is_empty()
        && value.chars().count() <= max_chars
        && !value.chars().any(char::is_control)
}

fn valid_free_form(value: &str) -> bool {
    !value.trim().is_empty()
        && value.chars().count() <= MAX_FREE_FORM_ANSWER_CHARS
        && !value
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\t'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Value {
        serde_json::json!({
            "questions": [{
                "id": "target",
                "header": "Target",
                "question": "Where should I deploy?",
                "options": [
                    {"id": "staging", "label": "Staging", "description": "Deploy for internal verification."},
                    {"id": "production", "label": "Production", "description": "Deploy to customers."}
                ],
                "allow_free_form": true
            }]
        })
    }

    #[test]
    fn contract_is_closed_bounded_and_unique() {
        assert!(validate_ask_user_questions_arguments(&sample()));
        let mut duplicate = sample();
        duplicate["questions"][0]["options"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "id": "staging",
                "label": "Other staging",
                "description": "Duplicate identity."
            }));
        assert!(!validate_ask_user_questions_arguments(&duplicate));
        let mut unknown = sample();
        unknown["questions"][0]["secret"] = Value::String("no".into());
        assert!(!validate_ask_user_questions_arguments(&unknown));
    }

    #[test]
    fn free_form_only_questions_must_opt_in() {
        assert!(!validate_ask_user_questions_arguments(&serde_json::json!({
            "questions": [{
                "id": "name",
                "header": "Name",
                "question": "What name should I use?"
            }]
        })));
        assert!(validate_ask_user_questions_arguments(&serde_json::json!({
            "questions": [{
                "id": "name",
                "header": "Name",
                "question": "What name should I use?",
                "allow_free_form": true
            }]
        })));
    }

    #[test]
    fn answers_require_exactly_one_value() {
        let option = UserQuestionAnswer {
            question_id: "target".into(),
            option_id: Some("staging".into()),
            free_form: None,
        };
        assert!(option.shape_is_well_formed());
        assert!(!UserQuestionAnswer {
            free_form: Some("both".into()),
            ..option
        }
        .shape_is_well_formed());
        assert!(UserQuestionAnswer {
            question_id: "note".into(),
            option_id: None,
            free_form: Some("First line\nSecond line".into()),
        }
        .shape_is_well_formed());
        assert!(!UserQuestionAnswer {
            question_id: "note".into(),
            option_id: None,
            free_form: Some("unsafe\0answer".into()),
        }
        .shape_is_well_formed());
    }
}
