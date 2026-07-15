//! Model-facing contracts for durable agent orchestration.
//!
//! These are prepared control-flow proposals, not generic server-executed
//! tools. The foreground turn worker owns the corresponding durable transition
//! so the model can never bypass its lease, steer, or accounting fences. The
//! production registry deliberately keeps this definition disabled until a
//! sandbox executor can claim and complete the child.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::AgentRun;
use crate::tool::ToolSpec;

/// Stable name for the foreground-only sandbox delegation tool.
pub const SPAWN_SANDBOX_AGENT_TOOL: &str = "spawn_sandbox_agent";

/// Maximum task length in Unicode scalar values advertised to a model.
///
/// The persisted byte limit remains [`AgentRun::MAX_INPUT_LEN`]; this lower
/// character cap ensures even four-byte UTF-8 input fits that durable bound.
pub const MAX_SANDBOX_AGENT_TASK_CHARS: usize = 16_000;

/// Canonical model proposal for one isolated sandbox task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpawnSandboxAgentArgs {
    /// A self-contained task for the isolated child. It cannot spawn children.
    pub task: String,
}

impl SpawnSandboxAgentArgs {
    /// Whether this proposal fits the durable sandbox-run contract.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        !self.task.trim().is_empty()
            && !self.task.contains('\0')
            && self.task.chars().count() <= MAX_SANDBOX_AGENT_TASK_CHARS
            && self.task.len() <= AgentRun::MAX_INPUT_LEN
    }
}

/// Validate one canonical model payload before the foreground worker parks.
#[must_use]
pub fn validate_spawn_sandbox_agent_arguments(arguments: &Value) -> bool {
    serde_json::from_value::<SpawnSandboxAgentArgs>(arguments.clone())
        .is_ok_and(|arguments| arguments.is_well_formed())
}

/// Foreground-only model tool contract for delegating one bounded task.
///
/// The worker derives the durable child identity from the model tool call and
/// atomically parks the foreground turn. Sandboxed agents never receive this
/// definition, so the v1 hierarchy cannot recurse past depth one.
#[must_use]
pub fn spawn_sandbox_agent_tool_spec() -> ToolSpec {
    ToolSpec {
        name: SPAWN_SANDBOX_AGENT_TOOL.into(),
        description: "Delegate one self-contained task to an isolated background agent. The conversation will pause until that agent returns. Use this only when independent work is useful; do not ask it to spawn more agents.".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "task": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": MAX_SANDBOX_AGENT_TASK_CHARS,
                    "description": "A concise, self-contained task for one isolated background agent."
                }
            },
            "required": ["task"],
            "additionalProperties": false
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_spawn_contract_is_strict_and_bounded() {
        let valid = serde_json::json!({"task": "Research the error handling approach."});
        assert!(validate_spawn_sandbox_agent_arguments(&valid));
        assert!(!validate_spawn_sandbox_agent_arguments(
            &serde_json::json!({
                "task": "",
            })
        ));
        assert!(!validate_spawn_sandbox_agent_arguments(
            &serde_json::json!({
                "task": "Research this.",
                "priority": "high",
            })
        ));
        assert!(!validate_spawn_sandbox_agent_arguments(
            &serde_json::json!({
                "task": format!("{}x", "a".repeat(MAX_SANDBOX_AGENT_TASK_CHARS)),
            })
        ));
    }

    #[test]
    fn sandbox_spawn_spec_describes_a_single_bounded_task() {
        let spec = spawn_sandbox_agent_tool_spec();
        assert_eq!(spec.name, SPAWN_SANDBOX_AGENT_TOOL);
        assert_eq!(spec.input_schema["additionalProperties"], false);
        assert_eq!(spec.input_schema["required"], serde_json::json!(["task"]));
        assert_eq!(spec.input_schema["properties"]["task"]["maxLength"], 16_000);
        assert!(spec.description.contains("do not ask it to spawn"));
    }
}
