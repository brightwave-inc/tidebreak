//! Model-facing contracts for durable agent orchestration.
//!
//! These are prepared control-flow proposals, not generic server-executed
//! tools. The foreground turn worker owns the corresponding durable transition
//! so the model can never bypass its lease, steer, or accounting fences. The
//! production registry deliberately keeps this definition disabled until a
//! sandbox executor can claim and complete the child.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;

use crate::id::AgentRunId;
use crate::model::{AgentRun, AgentRunResultPayload, TurnAgentRunWaitSet};
use crate::tool::ToolSpec;

/// Stable name for the foreground-only sandbox delegation tool.
pub const SPAWN_SANDBOX_AGENT_TOOL: &str = "spawn_sandbox_agent";
/// Stable name for the prepared foreground-only multi-child wait tool.
pub const WAIT_FOR_AGENTS_TOOL: &str = "wait_for_agents";
/// The only tool a depth-one sandbox may receive.
pub const SANDBOX_WEB_SEARCH_TOOL: &str = "web_search";

/// Maximum task length in Unicode scalar values advertised to a model.
///
/// The persisted byte limit remains [`AgentRun::MAX_INPUT_LEN`]; this lower
/// character cap ensures even four-byte UTF-8 input fits that durable bound.
pub const MAX_SANDBOX_AGENT_TASK_CHARS: usize = 16_000;
/// Maximum number of depth-one children in one foreground wait request.
pub const MAX_WAIT_FOR_AGENTS_CHILDREN: usize = TurnAgentRunWaitSet::MAX_CHILDREN;

/// Canonical model proposal for one isolated sandbox task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpawnSandboxAgentArgs {
    /// A self-contained task for the isolated child. It cannot spawn children.
    pub task: String,
}

/// Closed model-facing acknowledgement for a non-blocking sandbox spawn.
///
/// This deliberately excludes scheduler state and lease identities. The
/// foreground model needs only the stable child identity it may later pass to
/// [`WAIT_FOR_AGENTS_TOOL`]. The current runtime does not emit this result yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SpawnSandboxAgentResult {
    /// Stable identity of the admitted depth-one child.
    pub agent_id: AgentRunId,
}

/// Canonical model proposal to wait for an ordered set of sandbox children.
///
/// Completion always means `All`: the foreground turn resumes only after
/// every listed child has delivered an immutable result. The durable runtime
/// additionally verifies that each id belongs to a depth-one child owned by
/// the exact foreground turn; UUID shape alone cannot prove that authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WaitForAgentsArgs {
    /// Unique child identities in the order their results must be returned.
    pub agent_ids: Vec<AgentRunId>,
}

impl WaitForAgentsArgs {
    /// Whether this proposal has a non-empty, bounded, unique list of IDs.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        if self.agent_ids.is_empty()
            || self.agent_ids.len() > MAX_WAIT_FOR_AGENTS_CHILDREN
            || self.agent_ids.iter().any(|id| id.0.is_nil())
        {
            return false;
        }

        self.agent_ids.iter().copied().collect::<HashSet<_>>().len() == self.agent_ids.len()
    }
}

/// One closed, model-facing child result returned by [`WAIT_FOR_AGENTS_TOOL`].
///
/// Operational fencing data from the durable result receipt is intentionally
/// absent. Results remain in the caller's requested order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WaitForAgentResult {
    /// Child whose immutable result was delivered.
    pub agent_id: AgentRunId,
    /// Typed terminal payload produced by that child.
    pub result: AgentRunResultPayload,
}

/// Closed model-facing result for one all-children wait.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WaitForAgentsResult {
    /// One result per requested child, in the exact request order.
    pub results: Vec<WaitForAgentResult>,
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

/// Validate one canonical ordered all-children wait proposal.
#[must_use]
pub fn validate_wait_for_agents_arguments(arguments: &Value) -> bool {
    serde_json::from_value::<WaitForAgentsArgs>(arguments.clone())
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

/// Prepared foreground-only contract for waiting on depth-one children.
///
/// This definition is intentionally not registered yet. Advertising it is
/// safe only after non-blocking spawn and durable multi-child resume are wired
/// together in the foreground runtime.
#[must_use]
pub fn wait_for_agents_tool_spec() -> ToolSpec {
    ToolSpec {
        name: WAIT_FOR_AGENTS_TOOL.into(),
        description: "Wait until all specified background agents finish, then return their results in the same order. Use only depth-one agent IDs returned by spawn_sandbox_agent.".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "agent_ids": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": MAX_WAIT_FOR_AGENTS_CHILDREN,
                    "uniqueItems": true,
                    "items": { "type": "string", "format": "uuid" },
                    "description": "Opaque depth-one child agent IDs, in the order their results should be returned."
                }
            },
            "required": ["agent_ids"],
            "additionalProperties": false
        }),
    }
}

/// Narrow, host-executed web-search contract for an isolated sandbox run.
///
/// This is deliberately not registered in the foreground tool registry. The
/// sandbox worker checkpoints it under its own durable lease, and the host
/// decides whether any configured provider may execute it.
#[must_use]
pub fn sandbox_web_search_tool_spec() -> ToolSpec {
    ToolSpec {
        name: SANDBOX_WEB_SEARCH_TOOL.into(),
        description: "Search the public web for current information. Use at most once, with a focused query. Results may be unavailable when the host has not configured web search.".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "minLength": 1, "maxLength": 1024 },
                "max_results": { "type": "integer", "minimum": 1, "maximum": 10 },
                "domains": { "type": "array", "items": { "type": "string" }, "maxItems": 20 },
                "start_published_at": { "type": "string", "format": "date-time" },
                "end_published_at": { "type": "string", "format": "date-time" }
            },
            "required": ["query"],
            "additionalProperties": false
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn child_ids(count: usize) -> Vec<AgentRunId> {
        (0..count).map(|_| AgentRunId::new()).collect()
    }

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

    #[test]
    fn wait_for_agents_contract_accepts_an_ordered_bounded_unique_list() {
        let ids = child_ids(MAX_WAIT_FOR_AGENTS_CHILDREN);
        let arguments = serde_json::json!({"agent_ids": ids});

        assert!(validate_wait_for_agents_arguments(&arguments));
        let decoded: WaitForAgentsArgs = serde_json::from_value(arguments).unwrap();
        assert_eq!(decoded.agent_ids, ids);
    }

    #[test]
    fn wait_for_agents_contract_rejects_empty_duplicate_nil_and_oversized_lists() {
        let duplicate = AgentRunId::new();
        let oversized = child_ids(MAX_WAIT_FOR_AGENTS_CHILDREN + 1);

        for invalid in [
            serde_json::json!({"agent_ids": []}),
            serde_json::json!({"agent_ids": [duplicate, duplicate]}),
            serde_json::json!({"agent_ids": [uuid::Uuid::nil()]}),
            serde_json::json!({"agent_ids": oversized}),
        ] {
            assert!(!validate_wait_for_agents_arguments(&invalid), "{invalid}");
        }
    }

    #[test]
    fn wait_for_agents_contract_rejects_malformed_and_noncanonical_payloads() {
        let id = AgentRunId::new();

        for invalid in [
            serde_json::json!({}),
            serde_json::json!({"agent_ids": [id], "condition": "all"}),
            serde_json::json!({"agent_ids": "not-an-array"}),
            serde_json::json!({"agent_ids": ["not-a-uuid"]}),
            serde_json::json!({"agent_ids": [id, 42]}),
            serde_json::Value::Null,
        ] {
            assert!(!validate_wait_for_agents_arguments(&invalid), "{invalid}");
        }
    }

    #[test]
    fn wait_for_agents_spec_encodes_all_semantics_and_matches_validation_bound() {
        let spec = wait_for_agents_tool_spec();

        assert_eq!(spec.name, WAIT_FOR_AGENTS_TOOL);
        assert_eq!(spec.input_schema["additionalProperties"], false);
        assert_eq!(
            spec.input_schema["required"],
            serde_json::json!(["agent_ids"])
        );
        assert_eq!(spec.input_schema["properties"]["agent_ids"]["minItems"], 1);
        assert_eq!(
            spec.input_schema["properties"]["agent_ids"]["maxItems"],
            MAX_WAIT_FOR_AGENTS_CHILDREN
        );
        assert_eq!(
            spec.input_schema["properties"]["agent_ids"]["uniqueItems"],
            true
        );
        assert!(spec.description.contains("all specified"));
        assert!(spec.description.contains("same order"));
    }

    #[test]
    fn prepared_orchestration_results_have_closed_model_facing_shapes() {
        let agent_id = AgentRunId::new();
        let spawn = SpawnSandboxAgentResult { agent_id };
        assert_eq!(
            serde_json::to_value(spawn).unwrap(),
            serde_json::json!({"agent_id": agent_id})
        );
        let wait = WaitForAgentsResult {
            results: vec![WaitForAgentResult {
                agent_id,
                result: AgentRunResultPayload::FinalText {
                    text: "finished".into(),
                },
            }],
        };
        assert_eq!(
            serde_json::to_value(&wait).unwrap(),
            serde_json::json!({
                "results": [{
                    "agent_id": agent_id,
                    "result": {"kind": "final_text", "text": "finished"},
                }],
            })
        );
    }
}
