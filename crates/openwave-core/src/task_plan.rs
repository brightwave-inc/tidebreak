//! An agent's durable task plan.
//!
//! A plan is a short ordered checklist the agent keeps for itself while it
//! works through a request with several dependent steps. It is not a
//! continuation: [`UPDATE_TASK_PLAN_TOOL`] commits and the turn keeps going,
//! unlike [`crate::EXIT_PLAN_MODE_TOOL`] or [`crate::ASK_USER_QUESTIONS_TOOL`],
//! which park the turn on a reader's decision.
//!
//! Each call replaces the whole list. Sending the full list every time is what
//! makes the durable row a projection of one call rather than a fold over a
//! history of edits: nothing has to reconcile a partial update against what an
//! interrupted earlier call did or did not commit.
//!
//! Two agents keep plans and they are scoped differently. A foreground plan is
//! the conversation's, so it is keyed by chat and outlives the turn that wrote
//! it ([`TaskPlan`]). A background agent's plan belongs to the one delegated
//! task it was created for, and a chat may have many of those running at once,
//! so it is keyed by the run ([`AgentRunTaskPlan`]). Sharing one chat-keyed row
//! between them would let four sandbox siblings overwrite each other and the
//! conversation's own plan.

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{AgentRunId, ToolSpec, TurnId};

/// Stable foreground-only tool name.
pub const UPDATE_TASK_PLAN_TOOL: &str = "update_task_plan";

pub const MAX_TASK_PLAN_STEPS: usize = 20;
pub const MAX_TASK_PLAN_STEP_CHARS: usize = 500;

/// Where one step stands.
// The variants are deliberately undocumented: a `schemars` unit enum whose
// variants carry doc comments generates `oneOf` + `const` rather than a plain
// `enum` list, and the strict schema subset providers enforce has no form for
// the former. The meaning belongs on the field and in the tool description.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum TaskPlanStepStatus {
    Pending,
    InProgress,
    Completed,
}

/// One step of the plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[schemars(description = "")]
pub struct TaskPlanStep {
    /// What this step does, as one short imperative line.
    #[schemars(length(min = 1, max = MAX_TASK_PLAN_STEP_CHARS))]
    pub content: String,
    /// Where the step stands: `pending` before it starts, `in_progress` while
    /// it is being worked on (at most one step at a time), `completed` after.
    pub status: TaskPlanStepStatus,
}

/// Canonical model arguments for [`UPDATE_TASK_PLAN_TOOL`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateTaskPlanArgs {
    /// The complete plan, in the order the steps will be worked.
    #[schemars(length(min = 1, max = MAX_TASK_PLAN_STEPS))]
    pub steps: Vec<TaskPlanStep>,
}

/// Renderer-safe durable projection of a chat's current plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
pub struct TaskPlan {
    /// The turn whose call last replaced this plan.
    pub turn_id: TurnId,
    /// The steps, in order.
    pub steps: Vec<TaskPlanStep>,
    /// When the last replacement committed.
    pub updated_at: DateTime<Utc>,
}

/// Renderer-safe durable projection of one background run's current plan.
///
/// The run-scoped twin of [`TaskPlan`]. It carries no turn: a background run
/// is one delegated task from start to finish, so the run is the only scope
/// its plan ever had.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
pub struct AgentRunTaskPlan {
    pub run_id: AgentRunId,
    /// The steps, in order.
    pub steps: Vec<TaskPlanStep>,
    /// When the last replacement committed.
    pub updated_at: DateTime<Utc>,
}

/// Steps a plan has not finished, in order, for a reminder the model reads.
///
/// Empty when every step is `completed`, which is the only state in which a
/// run has nothing left to close out.
#[must_use]
pub fn open_task_plan_steps(steps: &[TaskPlanStep]) -> Vec<&str> {
    steps
        .iter()
        .filter(|step| step.status != TaskPlanStepStatus::Completed)
        .map(|step| step.content.as_str())
        .collect()
}

/// Parse and check one call's arguments, or say exactly what to fix.
///
/// The registry already rejects anything the advertised schema forbids, so the
/// message here only has to cover what a schema cannot express — chiefly the
/// single-`in_progress` rule, which is the whole reason a plan reads as
/// progress rather than as a list of intentions.
///
/// # Errors
///
/// Returns model-facing correction text when the arguments are unusable.
pub fn parse_update_task_plan_arguments(
    arguments: &Value,
) -> std::result::Result<UpdateTaskPlanArgs, String> {
    let parsed: UpdateTaskPlanArgs = serde_json::from_value(arguments.clone())
        .map_err(|error| format!("update_task_plan arguments are not valid: {error}"))?;
    check_steps(&parsed.steps)?;
    Ok(parsed)
}

fn check_steps(steps: &[TaskPlanStep]) -> std::result::Result<(), String> {
    if steps.is_empty() {
        return Err(
            "a task plan needs at least one step; send the whole list, or do not call this tool at all"
                .to_owned(),
        );
    }
    if steps.len() > MAX_TASK_PLAN_STEPS {
        return Err(format!(
            "a task plan holds at most {MAX_TASK_PLAN_STEPS} steps; you sent {}. Merge the smaller ones.",
            steps.len()
        ));
    }
    for (index, step) in steps.iter().enumerate() {
        if step.content.trim().is_empty() {
            return Err(format!(
                "step {} has empty content; every step needs one short line describing it",
                index + 1
            ));
        }
        if step.content.chars().count() > MAX_TASK_PLAN_STEP_CHARS {
            return Err(format!(
                "step {} is longer than {MAX_TASK_PLAN_STEP_CHARS} characters; shorten it to one line",
                index + 1
            ));
        }
        if step.content.chars().any(char::is_control) {
            return Err(format!(
                "step {} contains control characters; send plain single-line text",
                index + 1
            ));
        }
    }
    let in_progress = steps
        .iter()
        .filter(|step| step.status == TaskPlanStepStatus::InProgress)
        .count();
    if in_progress > 1 {
        return Err(format!(
            "{in_progress} steps are in_progress; exactly one step may be in_progress at a time. \
             Mark the finished ones completed and leave the rest pending."
        ));
    }
    Ok(())
}

/// A short model-facing receipt: what the committed plan now says.
#[must_use]
pub fn task_plan_summary(steps: &[TaskPlanStep]) -> String {
    let completed = steps
        .iter()
        .filter(|step| step.status == TaskPlanStepStatus::Completed)
        .count();
    let in_progress = steps
        .iter()
        .find(|step| step.status == TaskPlanStepStatus::InProgress)
        .map(|step| step.content.as_str());
    let total = steps.len();
    match in_progress {
        Some(current) => {
            format!(
                "Task plan updated: {completed}/{total} steps completed. Now working on: {current}"
            )
        }
        None => format!("Task plan updated: {completed}/{total} steps completed."),
    }
}

#[must_use]
pub fn update_task_plan_tool_spec() -> ToolSpec {
    ToolSpec::for_args::<UpdateTaskPlanArgs>(
        UPDATE_TASK_PLAN_TOOL,
        "Record the ordered steps you intend to work through, so the user can follow long work as it progresses. Send the complete list every time — each call replaces the previous plan. Keep exactly one step in_progress while you work on it, mark it completed as soon as it is done, and update the plan promptly rather than in a batch at the end. This tool returns immediately and does not pause the turn.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Value {
        serde_json::json!({
            "steps": [
                {"content": "Read the failing test", "status": "completed"},
                {"content": "Fix the parser", "status": "in_progress"},
                {"content": "Run the suite", "status": "pending"},
            ]
        })
    }

    #[test]
    fn the_contract_is_closed_bounded_and_single_threaded() {
        let parsed = parse_update_task_plan_arguments(&sample()).expect("the sample is valid");
        assert_eq!(parsed.steps.len(), 3);

        let mut two_active = sample();
        two_active["steps"][2]["status"] = Value::String("in_progress".into());
        let error = parse_update_task_plan_arguments(&two_active)
            .expect_err("two in_progress steps are refused");
        assert!(
            error.contains("in_progress"),
            "the correction must name the rule that was broken: {error}"
        );

        let mut unknown = sample();
        unknown["steps"][0]["note"] = Value::String("no".into());
        assert!(parse_update_task_plan_arguments(&unknown).is_err());

        assert!(parse_update_task_plan_arguments(&serde_json::json!({"steps": []})).is_err());

        let too_many = serde_json::json!({
            "steps": (0..=MAX_TASK_PLAN_STEPS)
                .map(|index| serde_json::json!({"content": format!("step {index}"), "status": "pending"}))
                .collect::<Vec<_>>()
        });
        assert!(parse_update_task_plan_arguments(&too_many).is_err());

        let mut blank = sample();
        blank["steps"][0]["content"] = Value::String("   ".into());
        assert!(parse_update_task_plan_arguments(&blank).is_err());
    }

    #[test]
    fn the_advertised_schema_carries_the_bounds_the_validator_enforces() {
        let schema = update_task_plan_tool_spec().input_schema;
        let steps = &schema["properties"]["steps"];
        let step = &steps["items"];

        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["required"], serde_json::json!(["steps"]));
        assert_eq!(steps["minItems"], 1);
        assert_eq!(steps["maxItems"], MAX_TASK_PLAN_STEPS);
        assert_eq!(step["additionalProperties"], false);
        assert_eq!(step["required"], serde_json::json!(["content", "status"]));
        assert_eq!(
            step["properties"]["content"]["maxLength"],
            MAX_TASK_PLAN_STEP_CHARS
        );
        // A plain `enum` list, not `oneOf`: the strict schema subset providers
        // enforce has no form for the latter.
        assert_eq!(
            step["properties"]["status"]["enum"],
            serde_json::json!(["pending", "in_progress", "completed"])
        );
        assert!(
            crate::tool::strict_json_schema(&schema, crate::tool::OptionalProperties::Reject)
                .is_some()
        );
    }
}
