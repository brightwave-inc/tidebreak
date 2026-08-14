//! Bounded foreground-agent questions and exact user answers.
//!
//! These contracts deliberately separate the canonical model proposal from the
//! renderer-facing durable projection. A question is a continuation, not a new
//! chat message: answering it completes the same tool call and resumes the
//! blocked turn.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
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
pub const MAX_ADDITIONAL_USER_CONTEXT_CHARS: usize = 2_000;

/// Whether the reader may select one option or several independent options.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "snake_case")]
pub enum UserQuestionType {
    #[default]
    SingleSelect,
    MultiSelect,
}

impl UserQuestionType {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SingleSelect => "single_select",
            Self::MultiSelect => "multi_select",
        }
    }
}

/// One selectable answer choice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[schemars(description = "")]
pub struct UserQuestionOption {
    #[schemars(length(min = 1, max = MAX_QUESTION_OPTION_ID_CHARS))]
    pub id: String,
    #[schemars(length(min = 1, max = MAX_QUESTION_OPTION_LABEL_CHARS))]
    pub label: String,
    #[schemars(length(min = 1, max = MAX_QUESTION_OPTION_DESCRIPTION_CHARS))]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[schemars(description = "")]
pub struct UserQuestion {
    #[schemars(length(min = 1, max = MAX_QUESTION_ID_CHARS))]
    pub id: String,
    #[schemars(length(min = 1, max = MAX_QUESTION_HEADER_CHARS))]
    pub header: String,
    #[schemars(length(min = 1, max = MAX_QUESTION_PROMPT_CHARS))]
    pub question: String,
    #[serde(default)]
    #[schemars(length(max = MAX_QUESTION_OPTIONS))]
    pub options: Vec<UserQuestionOption>,
    #[serde(default)]
    pub question_type: UserQuestionType,
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AskUserQuestionsArgs {
    #[schemars(length(min = 1, max = MAX_USER_QUESTIONS))]
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

/// One supplied answer. Omitted questions are explicitly skipped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserQuestionAnswer {
    pub question_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub selected_option_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_answer: Option<String>,
}

impl UserQuestionAnswer {
    #[must_use]
    pub fn shape_is_well_formed(&self) -> bool {
        if !valid_text(&self.question_id, MAX_QUESTION_ID_CHARS)
            || self.selected_option_ids.len() > MAX_QUESTION_OPTIONS
            || (self.selected_option_ids.is_empty() && self.custom_answer.is_none())
            || self
                .custom_answer
                .as_deref()
                .is_some_and(|answer| !valid_free_form(answer))
        {
            return false;
        }
        let mut option_ids = HashSet::with_capacity(self.selected_option_ids.len());
        self.selected_option_ids.iter().all(|option_id| {
            valid_text(option_id, MAX_QUESTION_OPTION_ID_CHARS)
                && option_ids.insert(option_id.as_str())
        })
    }
}

/// Exact answer command for a pending request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnswerUserQuestions {
    pub answers: Vec<UserQuestionAnswer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_user_context: Option<String>,
}

impl AnswerUserQuestions {
    #[must_use]
    pub fn shape_is_well_formed(&self) -> bool {
        if self.answers.len() > MAX_USER_QUESTIONS
            || self
                .additional_user_context
                .as_deref()
                .is_some_and(|context| !valid_additional_context(context))
        {
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
    ToolSpec::for_args::<AskUserQuestionsArgs>(
        ASK_USER_QUESTIONS_TOOL,
        "Pause the current foreground turn and ask the user up to three short structured questions. Use stable question and option IDs. Set question_type to single_select only when options are mutually exclusive; otherwise use multi_select. Enable allow_free_form when a custom answer is useful. The user may skip any or all questions, so proceed with reasonable defaults for omissions. Call this tool alone, with no assistant text or sibling tools. If the question call is reported as not run, correct the reported violation and issue a fresh standalone ask_user_questions call.",
    )
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

fn valid_additional_context(value: &str) -> bool {
    !value.trim().is_empty()
        && value.chars().count() <= MAX_ADDITIONAL_USER_CONTEXT_CHARS
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
        let description = ask_user_questions_tool_spec().description;
        assert!(description.contains("reported as not run"));
        assert!(description.contains("fresh standalone ask_user_questions call"));
    }

    #[test]
    fn question_schema_is_derived_with_all_nested_bounds() {
        let schema = ask_user_questions_tool_spec().input_schema;
        let questions = &schema["properties"]["questions"];
        let question = &questions["items"];
        let option = &question["properties"]["options"]["items"];

        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["required"], serde_json::json!(["questions"]));
        assert_eq!(questions["minItems"], 1);
        assert_eq!(questions["maxItems"], MAX_USER_QUESTIONS);
        assert_eq!(question["additionalProperties"], false);
        assert_eq!(
            question["required"],
            serde_json::json!(["id", "header", "question"])
        );
        assert_eq!(
            question["properties"]["question"]["maxLength"],
            MAX_QUESTION_PROMPT_CHARS
        );
        assert_eq!(
            question["properties"]["options"]["maxItems"],
            MAX_QUESTION_OPTIONS
        );
        assert_eq!(option["additionalProperties"], false);
        assert_eq!(
            option["required"],
            serde_json::json!(["id", "label", "description"])
        );
        assert_eq!(
            option["properties"]["description"]["maxLength"],
            MAX_QUESTION_OPTION_DESCRIPTION_CHARS
        );
        // What omitting an optional field means is part of the contract.
        assert_eq!(
            question["properties"]["question_type"]["default"],
            "single_select"
        );
        assert_eq!(question["properties"]["allow_free_form"]["default"], false);
        assert_eq!(
            question["properties"]["options"]["default"],
            serde_json::json!([])
        );
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
    fn answers_are_bounded_unique_and_may_be_skipped() {
        let option = UserQuestionAnswer {
            question_id: "target".into(),
            selected_option_ids: vec!["staging".into()],
            custom_answer: None,
        };
        assert!(option.shape_is_well_formed());
        assert!(!UserQuestionAnswer {
            selected_option_ids: vec!["staging".into(), "staging".into()],
            ..option.clone()
        }
        .shape_is_well_formed());
        assert!(UserQuestionAnswer {
            question_id: "note".into(),
            selected_option_ids: vec!["staging".into(), "production".into()],
            custom_answer: Some("First line\nSecond line".into()),
        }
        .shape_is_well_formed());
        assert!(!UserQuestionAnswer {
            question_id: "note".into(),
            selected_option_ids: Vec::new(),
            custom_answer: Some("unsafe\0answer".into()),
        }
        .shape_is_well_formed());
        assert!(AnswerUserQuestions {
            answers: Vec::new(),
            additional_user_context: None,
        }
        .shape_is_well_formed());
    }
}
