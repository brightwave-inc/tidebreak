//! Model-facing contracts for durable agent orchestration.
//!
//! These are prepared control-flow proposals, not generic server-executed
//! tools. The foreground turn worker owns the corresponding durable transition
//! so the model can never bypass its lease, steer, or accounting fences. The
//! production registry exposes them only to durably claimed foreground turns.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;

use crate::error::{AgentError, Result};
use crate::id::{AgentRunId, HostRootId};
use crate::model::{
    AgentRun, AgentRunInboxEntry, AgentRunResultPayload, ToolCallRecord, TurnAgentRunWaitSet,
};
use crate::tool::ToolSpec;

/// Stable name for the foreground-only sandbox delegation tool.
pub const SPAWN_SANDBOX_AGENT_TOOL: &str = "spawn_sandbox_agent";
/// Stable name for the prepared foreground-only multi-child wait tool.
pub const WAIT_FOR_AGENTS_TOOL: &str = "wait_for_agents";
/// Stable name for the provider-backed public-web search tool.
pub const WEB_SEARCH_TOOL: &str = "web_search";
/// Compatibility name for the same contract when checkpointed by a sandbox.
pub const SANDBOX_WEB_SEARCH_TOOL: &str = WEB_SEARCH_TOOL;
/// Stable name for the native-executed exact delegated file read.
pub const SANDBOX_READ_DELEGATED_FILE_TOOL: &str = "read_delegated_file";

/// Maximum task length in Unicode scalar values advertised to a model.
///
/// The persisted byte limit remains [`AgentRun::MAX_INPUT_LEN`]; this lower
/// character cap ensures even four-byte UTF-8 input fits that durable bound.
pub const MAX_SANDBOX_AGENT_TASK_CHARS: usize = 16_000;
/// Maximum number of depth-one children in one foreground wait request.
pub const MAX_WAIT_FOR_AGENTS_CHILDREN: usize = TurnAgentRunWaitSet::MAX_CHILDREN;

/// Maximum serialized JSON bytes allocated to one model-facing child result.
///
/// Four entries plus the fixed result-envelope overhead remain below the
/// durable tool-call result cap. The bound is on encoded JSON rather than
/// characters so control-character escaping and four-byte Unicode cannot
/// expand a valid child receipt into an unresumable wait.
pub const MAX_WAIT_FOR_AGENT_RESULT_JSON_BYTES: usize = 120 * 1024;
const WAIT_RESULT_TRUNCATION_MARKER: &str = "\n…[truncated for parent context]";
pub(crate) const WAIT_INTERRUPTED_BY_STEER_RESULT: &str =
    "Wait interrupted by a newer user message.";
pub(crate) const WAIT_CANCELLED_WITH_TURN_RESULT: &str =
    "Wait cancelled because the foreground turn was cancelled.";

/// Canonical model proposal for one isolated sandbox task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpawnSandboxAgentArgs {
    /// A self-contained task for the isolated child. It cannot spawn children.
    pub task: String,
    /// Optional exact file a compatible native executor may make available.
    ///
    /// Persisting this delegation grants no file access by itself. A compatible
    /// executor advertises the read only while the root remains attached, then
    /// revalidates authority with the host broker before bytes move.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<SandboxAgentFileResource>,
}

/// One exact, pathless-root file identity delegated to a sandbox child.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SandboxAgentFileResource {
    /// Opaque host-broker root identity attached to the foreground chat.
    pub root_id: HostRootId,
    /// Nonempty path relative to that root; absolute and parent paths are invalid.
    pub relative_path: String,
}

impl SandboxAgentFileResource {
    /// Whether this is a bounded, canonical root-relative file identity.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        crate::client_tools::valid_connected_folder_path(&self.relative_path, false)
    }
}

/// Closed model-facing acknowledgement for a non-blocking sandbox spawn.
///
/// This deliberately excludes scheduler state and lease identities. The
/// foreground model needs only the stable child identity it may later pass to
/// [`WAIT_FOR_AGENTS_TOOL`].
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
    /// Whether the parent-facing projection shortened the immutable payload.
    pub truncated: bool,
}

/// Closed model-facing result for one all-children wait.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WaitForAgentsResult {
    /// One result per requested child, in the exact request order.
    pub results: Vec<WaitForAgentResult>,
}

/// Build the canonical, bounded model-facing result for an ordered wait.
///
/// Immutable child receipts remain untouched. Only the projection inserted in
/// the foreground tool history may be shortened, and shortened text always
/// carries an explicit marker.
pub(crate) fn canonical_wait_for_agents_result(entries: &[AgentRunInboxEntry]) -> Result<String> {
    if entries.is_empty() || entries.len() > MAX_WAIT_FOR_AGENTS_CHILDREN {
        return Err(AgentError::Store(
            "ordered wait result has an invalid child count".into(),
        ));
    }
    let mut results = Vec::with_capacity(entries.len());
    for entry in entries {
        let original = WaitForAgentResult {
            agent_id: entry.child_run_id,
            result: entry.result.payload.clone(),
            truncated: false,
        };
        if serde_json::to_vec(&original)?.len() <= MAX_WAIT_FOR_AGENT_RESULT_JSON_BYTES {
            results.push(original);
            continue;
        }
        let AgentRunResultPayload::FinalText { text } = &entry.result.payload else {
            return Err(AgentError::Store(
                "non-text sandbox result exceeds its parent projection budget".into(),
            ));
        };
        results.push(WaitForAgentResult {
            agent_id: entry.child_run_id,
            result: AgentRunResultPayload::FinalText {
                text: truncate_wait_result_text(entry.child_run_id, text)?,
            },
            truncated: true,
        });
    }
    let result = serde_json::to_string(&WaitForAgentsResult { results })?;
    if result.len() > ToolCallRecord::MAX_RESULT_BYTES {
        return Err(AgentError::Store(
            "ordered wait result exceeds the durable tool-call result budget".into(),
        ));
    }
    Ok(result)
}

fn truncate_wait_result_text(agent_id: AgentRunId, text: &str) -> Result<String> {
    let boundaries = text
        .char_indices()
        .map(|(offset, _)| offset)
        .chain(std::iter::once(text.len()))
        .collect::<Vec<_>>();
    let fits = |end: usize| -> Result<bool> {
        let projected = WaitForAgentResult {
            agent_id,
            result: AgentRunResultPayload::FinalText {
                text: format!("{}{}", &text[..end], WAIT_RESULT_TRUNCATION_MARKER),
            },
            truncated: true,
        };
        Ok(serde_json::to_vec(&projected)?.len() <= MAX_WAIT_FOR_AGENT_RESULT_JSON_BYTES)
    };
    let mut low = 0;
    let mut high = boundaries.len();
    while low < high {
        let mid = low + (high - low).div_ceil(2);
        if fits(boundaries[mid - 1])? {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    let end = boundaries[low.saturating_sub(1)];
    if !fits(end)? {
        return Err(AgentError::Store(
            "ordered wait truncation marker exceeds its projection budget".into(),
        ));
    }
    Ok(format!("{}{}", &text[..end], WAIT_RESULT_TRUNCATION_MARKER))
}

impl SpawnSandboxAgentArgs {
    /// Whether this proposal fits the durable sandbox-run contract.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        !self.task.trim().is_empty()
            && !self.task.contains('\0')
            && self.task.chars().count() <= MAX_SANDBOX_AGENT_TASK_CHARS
            && self.task.len() <= AgentRun::MAX_INPUT_LEN
            && self
                .resource
                .as_ref()
                .is_none_or(SandboxAgentFileResource::is_well_formed)
    }
}

/// Validate one canonical model payload before the foreground worker parks.
#[must_use]
pub fn validate_spawn_sandbox_agent_arguments(arguments: &Value) -> bool {
    parse_canonical_spawn_sandbox_agent_arguments(arguments).is_some()
}

pub(crate) fn parse_canonical_spawn_sandbox_agent_arguments(
    arguments: &Value,
) -> Option<SpawnSandboxAgentArgs> {
    let decoded = serde_json::from_value::<SpawnSandboxAgentArgs>(arguments.clone()).ok()?;
    (decoded.is_well_formed() && serde_json::to_value(&decoded).ok().as_ref() == Some(arguments))
        .then_some(decoded)
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
/// atomically checkpoints the result before continuing the foreground turn.
/// Sandboxed agents never receive this definition, so the v1 hierarchy cannot
/// recurse past depth one.
#[must_use]
pub fn spawn_sandbox_agent_tool_spec() -> ToolSpec {
    ToolSpec {
        name: SPAWN_SANDBOX_AGENT_TOOL.into(),
        description: "Delegate one self-contained task to an isolated background agent and continue immediately. Save every returned agent_id. Before final completion, include every spawned agent_id in a wait_for_agents call; batch independent IDs into one wait, and do not ask it to spawn more agents.".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "task": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": MAX_SANDBOX_AGENT_TASK_CHARS,
                    "description": "A concise, self-contained task for one isolated background agent."
                },
                "resource": {
                    "type": "object",
                    "description": "Optional exact file identity within a folder already connected to this conversation. This delegates only its identity; a compatible native executor must revalidate current access before any read.",
                    "properties": {
                        "root_id": { "type": "string", "format": "uuid" },
                        "relative_path": {
                            "type": "string",
                            "minLength": 1,
                            "maxLength": crate::client_tools::MAX_CONNECTED_FOLDER_PATH_BYTES,
                            "description": "Nonempty root-relative file path."
                        }
                    },
                    "required": ["root_id", "relative_path"],
                    "additionalProperties": false
                }
            },
            "required": ["task"],
            "additionalProperties": false
        }),
    }
}

/// Prepared foreground-only contract for waiting on depth-one children.
///
/// It is advertised only with the matching non-blocking spawn contract from a
/// durably claimed foreground turn.
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

/// Narrow, host-executed web-search contract shared by trusted runtimes.
///
/// The provider-backed implementation belongs to `openwave-web-search`; core
/// owns this schema because the sandbox checkpoint protocol must advertise the
/// same closed arguments without depending on a network integration crate.
#[must_use]
pub fn web_search_tool_spec() -> ToolSpec {
    ToolSpec {
        name: WEB_SEARCH_TOOL.into(),
        description: "Search the public web for current information. Use focused queries and cite sources with the exact result URLs. Results may be unavailable when the host has not configured web search.".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "minLength": 1, "maxLength": 400 },
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

/// Sandbox-specific presentation of the shared web-search contract.
///
/// The checkpoint worker, not the ordinary tool registry, executes this
/// definition under its own durable lease. The one-call instruction matches
/// the sandbox state machine's admission bound.
#[must_use]
pub fn sandbox_web_search_tool_spec() -> ToolSpec {
    let mut spec = web_search_tool_spec();
    spec.description = "Search the public web for current information. Use at most once, with a focused query, and cite sources with the exact result URLs. Results may be unavailable when the host has not configured web search.".into();
    spec
}

/// Native-executed contract for reading the exact file delegated at spawn.
///
/// The model supplies no path or root: both identities are recovered from the
/// immutable child admission and revalidated against the chat attachment at
/// claim time. Keeping the schema argument-free prevents a sandbox from
/// widening its own authority.
#[must_use]
pub fn sandbox_read_delegated_file_tool_spec() -> ToolSpec {
    ToolSpec {
        name: SANDBOX_READ_DELEGATED_FILE_TOOL.into(),
        description: "Read the exact UTF-8 file delegated to this background task.".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }),
    }
}

/// Whether arguments are the one canonical argument-free payload.
#[must_use]
pub fn validate_sandbox_read_delegated_file_arguments(arguments: &Value) -> bool {
    arguments.as_object().is_some_and(serde_json::Map::is_empty)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn child_ids(count: usize) -> Vec<AgentRunId> {
        (0..count).map(|_| AgentRunId::new()).collect()
    }

    fn inbox(agent_id: AgentRunId, text: String) -> AgentRunInboxEntry {
        let now = Utc::now();
        AgentRunInboxEntry {
            parent_run_id: AgentRunId::new(),
            child_run_id: agent_id,
            chat_id: crate::ChatId::new(),
            result: crate::AgentRunResult {
                agent_run_id: agent_id,
                lease_token: uuid::Uuid::new_v4(),
                attempt_count: 1,
                claim_count: 1,
                payload: AgentRunResultPayload::FinalText { text: text.clone() },
                text,
                submitted_at: now,
            },
            status: crate::AgentRunInboxStatus::Pending,
            claim_count: 0,
            lease_token: None,
            lease_expires_at: None,
            consumed_lease_token: None,
            consumed_at: None,
            delivered_at: now,
        }
    }

    #[test]
    fn sandbox_spawn_contract_is_strict_and_bounded() {
        let valid = serde_json::json!({"task": "Research the error handling approach."});
        assert!(validate_spawn_sandbox_agent_arguments(&valid));
        assert!(!validate_spawn_sandbox_agent_arguments(
            &serde_json::json!({
                "task": "Research without a delegated file.",
                "resource": null
            })
        ));
        assert!(validate_spawn_sandbox_agent_arguments(&serde_json::json!({
            "task": "Inspect the exact report.",
            "resource": {
                "root_id": uuid::Uuid::new_v4(),
                "relative_path": "reports/summary.md"
            }
        })));
        assert!(!validate_spawn_sandbox_agent_arguments(
            &serde_json::json!({
                "task": "",
            })
        ));
        assert!(!validate_spawn_sandbox_agent_arguments(
            &serde_json::json!({
                "task": "Escape the connected root.",
                "resource": {
                    "root_id": uuid::Uuid::new_v4(),
                    "relative_path": "../private.txt"
                }
            })
        ));
        assert!(!validate_spawn_sandbox_agent_arguments(
            &serde_json::json!({
                "task": "Use a partial resource.",
                "resource": { "root_id": uuid::Uuid::new_v4() }
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
        assert_eq!(
            spec.input_schema["properties"]["resource"]["required"],
            serde_json::json!(["root_id", "relative_path"])
        );
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
                truncated: false,
            }],
        };
        assert_eq!(
            serde_json::to_value(&wait).unwrap(),
            serde_json::json!({
                "results": [{
                    "agent_id": agent_id,
                    "result": {"kind": "final_text", "text": "finished"},
                    "truncated": false,
                }],
            })
        );
    }

    #[test]
    fn delegated_file_read_contract_is_fixed_and_argument_free() {
        let spec = sandbox_read_delegated_file_tool_spec();
        assert_eq!(spec.name, SANDBOX_READ_DELEGATED_FILE_TOOL);
        assert_eq!(spec.input_schema["properties"], serde_json::json!({}));
        assert!(validate_sandbox_read_delegated_file_arguments(
            &serde_json::json!({})
        ));
        assert!(!validate_sandbox_read_delegated_file_arguments(
            &serde_json::json!({"relative_path": "notes.txt"})
        ));
        assert!(!validate_sandbox_read_delegated_file_arguments(
            &serde_json::Value::Null
        ));
    }

    #[test]
    fn foreground_and_sandbox_web_search_share_one_bounded_schema() {
        let foreground = web_search_tool_spec();
        let sandbox = sandbox_web_search_tool_spec();

        assert_eq!(foreground.name, WEB_SEARCH_TOOL);
        assert_eq!(sandbox.input_schema, foreground.input_schema);
        assert_eq!(
            foreground.input_schema["properties"]["query"]["maxLength"],
            400
        );
        assert_eq!(
            foreground.input_schema["properties"]["max_results"]["maximum"],
            10
        );
        assert_eq!(
            foreground.input_schema["properties"]["domains"]["maxItems"],
            20
        );
        assert_eq!(foreground.input_schema["additionalProperties"], false);
        assert!(foreground.description.contains("exact result URLs"));
        assert!(sandbox.description.contains("at most once"));
    }

    #[test]
    fn wait_result_projection_bounds_worst_case_json_escaping_without_mutating_receipts() {
        let entries = (0..MAX_WAIT_FOR_AGENTS_CHILDREN)
            .map(|_| inbox(AgentRunId::new(), "\u{1}".repeat(AgentRun::MAX_RESULT_LEN)))
            .collect::<Vec<_>>();
        let original = entries[0].result.payload.clone();

        let encoded = canonical_wait_for_agents_result(&entries).unwrap();
        assert!(encoded.len() <= ToolCallRecord::MAX_RESULT_BYTES);
        assert_eq!(entries[0].result.payload, original);
        let value: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(value["results"].as_array().unwrap().len(), 4);
        for result in value["results"].as_array().unwrap() {
            assert_eq!(result["truncated"], true);
            assert!(result["result"]["text"]
                .as_str()
                .unwrap()
                .ends_with(WAIT_RESULT_TRUNCATION_MARKER));
        }
    }

    #[test]
    fn wait_result_projection_truncates_only_at_unicode_boundaries() {
        let text = "🧭".repeat(AgentRun::MAX_RESULT_LEN);
        let entries = (0..MAX_WAIT_FOR_AGENTS_CHILDREN)
            .map(|_| inbox(AgentRunId::new(), text.clone()))
            .collect::<Vec<_>>();

        let encoded = canonical_wait_for_agents_result(&entries).unwrap();
        assert!(encoded.len() <= ToolCallRecord::MAX_RESULT_BYTES);
        let value: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        for result in value["results"].as_array().unwrap() {
            assert_eq!(result["truncated"], true);
            let projected = result["result"]["text"].as_str().unwrap();
            assert!(projected.ends_with(WAIT_RESULT_TRUNCATION_MARKER));
            assert!(projected
                .trim_end_matches(WAIT_RESULT_TRUNCATION_MARKER)
                .chars()
                .all(|character| character == '🧭'));
        }
    }
}
