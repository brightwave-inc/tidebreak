//! Model-facing contracts for durable agent orchestration.
//!
//! These are prepared control-flow proposals, not generic server-executed
//! tools. The foreground turn worker owns the corresponding durable transition
//! so the model can never bypass its lease, steer, or accounting fences. The
//! production registry exposes them only to durably claimed foreground turns.

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
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
/// Stable name for the foreground model's terminal blocked-outcome declaration.
pub const REPORT_BLOCKED_TOOL: &str = "report_blocked";
/// Stable name for the provider-backed public-web search tool.
pub const WEB_SEARCH_TOOL: &str = "web_search";
/// Stable name for the single-page public-web extraction tool.
pub const WEB_EXTRACT_TOOL: &str = "web_extract";
/// Compatibility name for the same contract when checkpointed by a sandbox.
pub const SANDBOX_WEB_SEARCH_TOOL: &str = WEB_SEARCH_TOOL;
/// Stable name for the native-executed exact delegated file read.
pub const SANDBOX_READ_DELEGATED_FILE_TOOL: &str = "read_delegated_file";
/// Stable name for command execution inside a background run's own workspace.
///
/// Deliberately the same name a foreground turn sees: it is the same operation
/// against the same kind of private workspace, and a model that has learned one
/// vocabulary should not have to learn a second one when it is delegated work.
pub const SANDBOX_EXEC_TOOL: &str = "exec";
/// Stable name for a background run's terminal submission of its own files.
pub const SANDBOX_DONE_TOOL: &str = "done";

/// Maximum task length in Unicode scalar values advertised to a model.
///
/// The persisted byte limit remains [`AgentRun::MAX_INPUT_LEN`]; this lower
/// character cap ensures even four-byte UTF-8 input fits that durable bound.
pub const MAX_SANDBOX_AGENT_TASK_CHARS: usize = 16_000;
/// Maximum number of depth-one children in one foreground wait request.
pub const MAX_WAIT_FOR_AGENTS_CHILDREN: usize = TurnAgentRunWaitSet::MAX_CHILDREN;
/// Maximum machine-readable reason-code length for [`REPORT_BLOCKED_TOOL`].
pub const MAX_REPORT_BLOCKED_REASON_CODE_CHARS: usize = 64;
/// Maximum user-facing blocked explanation length for [`REPORT_BLOCKED_TOOL`].
pub const MAX_REPORT_BLOCKED_EXPLANATION_CHARS: usize = 2_000;
/// Default model steps a sandbox run may take before it must wrap up.
///
/// This is the run's one policy bound — a check-in cadence, not a row budget.
/// Each step is one model completion over the replayed checkpoint chain, so
/// the cadence bounds context growth and model spend at once; durable rows
/// need no bound of their own because they cannot accumulate without steps
/// accumulating. Reaching the cadence is not a failure: the run's tools are
/// withdrawn two steps early, the request says so in words, and the run ends
/// by submitting what it has. The value is user-configurable in settings;
/// this constant is the default when no setting is stored.
pub const DEFAULT_SANDBOX_AGENT_CHECKIN_STEPS: usize = 100;
/// Default consecutive tool-call errors after which a sandbox run checks in.
///
/// A run whose worker keeps crashing is caught by `max_attempts`, but a run
/// whose tool calls keep *failing* looks healthy to the scheduler — every
/// step completes, every row resolves, just always as an error. Left alone it
/// thrashes until the step cadence ends it. This is the earlier trigger: N
/// trailing error receipts in a row, reset by any success. Derived from
/// durable receipts, so a replayed claim computes the same answer.
pub const DEFAULT_SANDBOX_AGENT_ERROR_CHECKIN: usize = 5;

/// Whether a sandbox tool call may run alongside its batch siblings.
///
/// Every tool in a step is dispatched as soon as it is parked except `exec`,
/// which mutates the run's one shared workspace: two commands running at once
/// would race over the same files, and the model wrote them expecting the
/// earlier one's effects. Execs therefore serialize in the order the step
/// emitted them, while read-only tools — web search, delegated file reads —
/// are claimed the moment they land.
#[must_use]
pub fn sandbox_call_is_parallel_eligible(name: &str) -> bool {
    name != SANDBOX_EXEC_TOOL
}
/// Maximum web-search query length advertised to a model.
pub const MAX_WEB_SEARCH_QUERY_CHARS: usize = 400;
/// Maximum requested web-search result count.
pub const MAX_WEB_SEARCH_RESULTS: usize = 10;
/// Maximum number of web-search domain filters.
pub const MAX_WEB_SEARCH_DOMAINS: usize = 20;
/// Maximum characters in one normalized web-search domain filter.
pub const MAX_WEB_SEARCH_DOMAIN_CHARS: usize = 253;
/// Default result count when a model omits `max_results`.
pub const DEFAULT_WEB_SEARCH_RESULTS: usize = 5;
/// Maximum title length accepted from one provider-executed search result.
///
/// Kept equal to the host search normalizer's result-title bound. Provider
/// receipts claim to already have that host shape, so overlong values are not
/// truncated a second time at ingress; the whole non-canonical receipt is
/// excluded instead.
pub(crate) const MAX_PROVIDER_WEB_SEARCH_TITLE_CHARS: usize = 300;
/// Maximum snippet length accepted from one provider-executed search result.
pub(crate) const MAX_PROVIDER_WEB_SEARCH_SNIPPET_CHARS: usize = 2_000;
/// Maximum optional extracted-content length in a normalized search result.
pub(crate) const MAX_PROVIDER_WEB_SEARCH_CONTENT_CHARS: usize = 4_000;
/// Maximum serialized bytes in a normalized provider-executed search result.
pub(crate) const MAX_PROVIDER_WEB_SEARCH_OUTPUT_BYTES: usize = 16_000;
/// Maximum native replay bytes retained beside one provider-executed search.
const MAX_PROVIDER_WEB_SEARCH_REPLAY_BYTES: usize = ToolCallRecord::MAX_RESULT_BYTES;
/// Maximum provider-native blocks in one search replay receipt.
const MAX_PROVIDER_WEB_SEARCH_REPLAY_BLOCKS: usize = 8;
const MAX_PROVIDER_WEB_SEARCH_METADATA_ENTRIES: usize = 8;
const MAX_PROVIDER_WEB_SEARCH_METADATA_KEY_CHARS: usize = 64;
const MAX_PROVIDER_WEB_SEARCH_METADATA_VALUE_CHARS: usize = 256;
/// Maximum executable length advertised to a sandboxed background agent.
///
/// The three sandbox-exec bounds mirror the ones `tidebreak-code-execution`
/// enforces at the provider boundary. They are restated here because the schema
/// a background agent sees is core's to publish, and the executor revalidates
/// every field before a command runs.
pub const MAX_SANDBOX_EXEC_COMMAND_BYTES: usize = 1_024;
/// Maximum argument-vector length advertised to a sandboxed background agent.
pub const MAX_SANDBOX_EXEC_ARGUMENTS: usize = 128;
/// Maximum working-directory length advertised to a sandboxed background agent.
pub const MAX_SANDBOX_EXEC_CWD_BYTES: usize = 1_024;
/// Maximum number of files one background run may submit through `done`.
pub const MAX_SANDBOX_DONE_OUTPUTS: usize = 16;
/// Maximum summary length in Unicode scalar values a submission may carry.
///
/// The summary is prose the parent turn reads beside the submitted filenames,
/// not the deliverable itself. Bounding it well under [`AgentRun::MAX_RESULT_LEN`]
/// keeps a multi-child wait result inside its own serialized ceiling and keeps
/// the pressure where it belongs: on the files, not on the description of them.
pub const MAX_SANDBOX_DONE_SUMMARY_CHARS: usize = 4_000;
/// Maximum web-extract URL length in bytes advertised to a model.
///
/// `tidebreak-server::web_search` holds its fetch-admission URL bound to this value, so
/// the schema a model sees and the policy the fetcher enforces cannot drift.
pub const MAX_WEB_EXTRACT_URL_BYTES: usize = 2_048;

/// Maximum serialized JSON bytes allocated to one model-facing child result.
///
/// The parent sees a bounded summary plus submitted output paths, not the
/// child's full write-up. Immutable receipts are untouched. The bound is on
/// encoded JSON rather than characters so control-character escaping and
/// four-byte Unicode cannot expand a valid child receipt into an unresumable
/// wait. Four entries plus the result envelope remain below the durable
/// tool-call result cap.
pub const MAX_WAIT_FOR_AGENT_RESULT_JSON_BYTES: usize = 8 * 1024;
const WAIT_RESULT_TRUNCATION_MARKER: &str = "\n…[truncated for parent context]";
pub(crate) const WAIT_INTERRUPTED_BY_STEER_RESULT: &str =
    "Wait interrupted by a newer user message.";
pub(crate) const WAIT_CANCELLED_WITH_TURN_RESULT: &str =
    "Wait cancelled because the foreground turn was cancelled.";

/// Canonical model proposal for one isolated sandbox task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SpawnSandboxAgentArgs {
    /// A self-contained task for the isolated child. It cannot spawn children.
    #[schemars(
        length(min = 1, max = MAX_SANDBOX_AGENT_TASK_CHARS),
        description = "A concise, self-contained task for one isolated background agent."
    )]
    pub task: String,
    /// Optional exact file a compatible native executor may make available.
    ///
    /// Persisting this delegation grants no file access by itself. A compatible
    /// executor advertises the read only while the root remains attached, then
    /// revalidates authority with the host broker before bytes move.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(
        with = "SandboxAgentFileResource",
        description = "Optional exact file identity within a folder already connected to this conversation. This delegates only its identity; a compatible native executor must revalidate current access before any read."
    )]
    pub resource: Option<SandboxAgentFileResource>,
}

/// One exact, pathless-root file identity delegated to a sandbox child.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SandboxAgentFileResource {
    /// Opaque host-broker root identity attached to the foreground chat.
    #[schemars(with = "uuid::Uuid", description = "")]
    pub root_id: HostRootId,
    /// Nonempty path relative to that root; absolute and parent paths are invalid.
    #[schemars(
        length(min = 1, max = crate::client_tools::MAX_CONNECTED_FOLDER_PATH_BYTES),
        description = "Nonempty root-relative file path."
    )]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WaitForAgentsArgs {
    /// Unique child identities in the order their results must be returned.
    #[schemars(
        with = "Vec<uuid::Uuid>",
        length(min = 1, max = MAX_WAIT_FOR_AGENTS_CHILDREN),
        extend("uniqueItems" = true),
        description = "Opaque depth-one child agent IDs, in the order their results should be returned."
    )]
    pub agent_ids: Vec<AgentRunId>,
}

/// Canonical arguments for the shared provider-backed web-search contract.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WebSearchArgs {
    /// The model's own account of what this call is doing, shown to the user
    /// in place of the raw arguments.
    ///
    /// Required in the schema so a model reliably writes one, `Option` here so
    /// a call that omits it still runs. Display-only: see
    /// [`crate::ToolActionPreview`].
    #[schemars(
        required,
        length(max = crate::preview::MAX_ACTION_SUMMARY_CHARS),
        description = crate::SUMMARY_ARGUMENT_DESCRIPTION
    )]
    pub summary: Option<String>,
    /// Focused public-web query.
    #[schemars(
        length(min = 1, max = MAX_WEB_SEARCH_QUERY_CHARS),
        description = ""
    )]
    pub query: String,
    /// Requested result count.
    #[serde(default = "default_web_search_results")]
    #[schemars(
        range(min = 1, max = MAX_WEB_SEARCH_RESULTS),
        description = ""
    )]
    pub max_results: usize,
    /// Optional exact domain filters.
    #[serde(default)]
    #[schemars(length(max = MAX_WEB_SEARCH_DOMAINS), description = "")]
    pub domains: Vec<String>,
    /// Optional inclusive lower publication-time bound.
    #[serde(default)]
    #[schemars(
        with = "String",
        extend("format" = "date-time"),
        description = ""
    )]
    pub start_published_at: Option<DateTime<Utc>>,
    /// Optional inclusive upper publication-time bound.
    #[serde(default)]
    #[schemars(
        with = "String",
        extend("format" = "date-time"),
        description = ""
    )]
    pub end_published_at: Option<DateTime<Utc>>,
}

const fn default_web_search_results() -> usize {
    DEFAULT_WEB_SEARCH_RESULTS
}

/// Canonical arguments for the single-page web-extraction contract.
///
/// One exact URL per call is the whole argument surface: no fetch options, no
/// depth, no output shaping. Everything else about a fetch — admission policy,
/// timeout, redirect handling, output budget — is host-owned.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WebExtractArgs {
    /// The model's own account of what this call is doing, shown to the user
    /// in place of the raw arguments.
    ///
    /// Required in the schema so a model reliably writes one, `Option` here so
    /// a call that omits it still runs. Display-only: see
    /// [`crate::ToolActionPreview`].
    #[schemars(
        required,
        length(max = crate::preview::MAX_ACTION_SUMMARY_CHARS),
        description = crate::SUMMARY_ARGUMENT_DESCRIPTION
    )]
    pub summary: Option<String>,
    /// Exact public https page URL to fetch and extract.
    #[schemars(
        length(min = 1, max = MAX_WEB_EXTRACT_URL_BYTES),
        description = ""
    )]
    pub url: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SandboxReadDelegatedFileArgs {}

/// Canonical arguments for one command inside a background run's own workspace.
///
/// This is the sandbox presentation of the foreground `exec` contract, narrowed
/// to what a background run can legitimately ask for: no staged host paths and
/// no folder authority, because a background run's workspace holds nothing but
/// what its own earlier commands wrote.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SandboxExecArgs {
    /// The model's own account of what this call is doing, shown to the user
    /// in place of the raw arguments.
    ///
    /// Required in the schema so a model reliably writes one, `Option` here so
    /// a call that omits it still runs. Display-only: see
    /// [`crate::ToolActionPreview`].
    #[schemars(
        required,
        length(max = crate::preview::MAX_ACTION_SUMMARY_CHARS),
        description = crate::SUMMARY_ARGUMENT_DESCRIPTION
    )]
    pub summary: Option<String>,
    /// Executable name or path (argv[0] only — no spaces or shell syntax).
    #[schemars(
        length(min = 1, max = MAX_SANDBOX_EXEC_COMMAND_BYTES),
        description = "Single executable name or path (argv[0]). No spaces; put flags and operands in args."
    )]
    pub command: String,
    /// Arguments passed directly to the executable, with no shell parsing.
    #[serde(default)]
    #[schemars(
        length(max = MAX_SANDBOX_EXEC_ARGUMENTS),
        description = "One argv entry per token, with no shell parsing (e.g. [\"-p\", \"output\"] for mkdir)."
    )]
    pub args: Vec<String>,
    /// Workspace-relative working directory.
    #[serde(default = "default_sandbox_exec_cwd")]
    #[schemars(
        length(min = 1, max = MAX_SANDBOX_EXEC_CWD_BYTES),
        description = "Workspace-relative working directory (defaults to '.')."
    )]
    pub cwd: String,
}

fn default_sandbox_exec_cwd() -> String {
    ".".into()
}

/// Canonical arguments for a background run's terminal submission.
///
/// The model names files, not identities: every entry is the filename of a file
/// the run already wrote under `output/`, which the host published as an output
/// under that same name. Nothing here creates an output — submission only marks
/// which of the run's published files are the deliverables the task asked for.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SandboxDoneArgs {
    /// Filenames of files written under `output/` that answer the task.
    #[serde(default)]
    #[schemars(
        length(max = MAX_SANDBOX_DONE_OUTPUTS),
        description = "Filenames of files you wrote under output/ that are the deliverables for this task."
    )]
    pub outputs: Vec<String>,
    /// Short prose describing what was produced.
    #[schemars(
        length(min = 1, max = MAX_SANDBOX_DONE_SUMMARY_CHARS),
        description = "Short summary of what you produced and anything the reader should know. Do not restate the file contents here."
    )]
    pub summary: String,
}

impl SandboxDoneArgs {
    /// Whether every field is within the advertised bounds.
    ///
    /// Filenames are checked against the same portable-filename rule the output
    /// scan applies, so a submission can only ever name something the scan could
    /// have published.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        self.outputs.len() <= MAX_SANDBOX_DONE_OUTPUTS
            && self
                .outputs
                .iter()
                .all(|name| crate::validate_portable_filename(name).is_ok())
            && self.outputs.iter().collect::<HashSet<_>>().len() == self.outputs.len()
            && !self.summary.trim().is_empty()
            && self.summary.chars().count() <= MAX_SANDBOX_DONE_SUMMARY_CHARS
    }
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
        let text = match &entry.result.payload {
            AgentRunResultPayload::FinalText { text }
            | AgentRunResultPayload::Submission { summary: text, .. }
            | AgentRunResultPayload::CheckIn { detail: text, .. } => text.as_str(),
            _ => {
                return Err(AgentError::Store(
                    "non-text sandbox result exceeds its parent projection budget".into(),
                ));
            }
        };
        results.push(WaitForAgentResult {
            agent_id: entry.child_run_id,
            result: wait_result_with_text(
                &entry.result.payload,
                truncate_wait_result_text(entry.child_run_id, &entry.result.payload, text)?,
            ),
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

fn wait_result_with_text(payload: &AgentRunResultPayload, text: String) -> AgentRunResultPayload {
    match payload {
        AgentRunResultPayload::FinalText { .. } => AgentRunResultPayload::FinalText { text },
        AgentRunResultPayload::Submission { outputs, .. } => AgentRunResultPayload::Submission {
            outputs: outputs.clone(),
            summary: text,
        },
        AgentRunResultPayload::CheckIn {
            reason, steps_used, ..
        } => AgentRunResultPayload::CheckIn {
            reason: *reason,
            steps_used: *steps_used,
            detail: text,
        },
        other => other.clone(),
    }
}

fn truncate_wait_result_text(
    agent_id: AgentRunId,
    payload: &AgentRunResultPayload,
    text: &str,
) -> Result<String> {
    let boundaries = text
        .char_indices()
        .map(|(offset, _)| offset)
        .chain(std::iter::once(text.len()))
        .collect::<Vec<_>>();
    let fits = |end: usize| -> Result<bool> {
        let projected = WaitForAgentResult {
            agent_id,
            result: wait_result_with_text(
                payload,
                format!("{}{}", &text[..end], WAIT_RESULT_TRUNCATION_MARKER),
            ),
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

/// Canonical terminal declaration that a mandatory outcome cannot be delivered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReportBlockedArgs {
    /// Short diagnostic identifier retained on the durable tool call.
    #[schemars(length(min = 1, max = MAX_REPORT_BLOCKED_REASON_CODE_CHARS))]
    pub reason_code: String,
    /// Concise explanation shown to the user as the assistant's final output.
    #[schemars(length(min = 1, max = MAX_REPORT_BLOCKED_EXPLANATION_CHARS))]
    pub explanation: String,
}

impl ReportBlockedArgs {
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        !self.reason_code.is_empty()
            && self.reason_code.chars().count() <= MAX_REPORT_BLOCKED_REASON_CODE_CHARS
            && self
                .reason_code
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
            && self.explanation == self.explanation.trim()
            && !self.explanation.is_empty()
            && self.explanation.chars().count() <= MAX_REPORT_BLOCKED_EXPLANATION_CHARS
            && !self
                .explanation
                .chars()
                .any(|character| character.is_control() && !matches!(character, '\n' | '\t'))
    }
}

/// Parse and validate one canonical blocked declaration.
#[must_use]
pub fn parse_report_blocked_arguments(arguments: &Value) -> Option<ReportBlockedArgs> {
    let decoded = serde_json::from_value::<ReportBlockedArgs>(arguments.clone()).ok()?;
    decoded.is_well_formed().then_some(decoded)
}

/// Validate one canonical blocked declaration before terminal state changes.
#[must_use]
pub fn validate_report_blocked_arguments(arguments: &Value) -> bool {
    parse_report_blocked_arguments(arguments).is_some()
}

/// Foreground-only terminal control for an impossible mandatory outcome.
#[must_use]
pub fn report_blocked_tool_spec() -> ToolSpec {
    ToolSpec::for_args::<ReportBlockedArgs>(
        REPORT_BLOCKED_TOOL,
        "End the foreground turn unsuccessfully when a mandatory requested outcome is impossible after available recovery paths were attempted or shown unavailable. Provide a short lowercase reason_code and a concise user-facing explanation. The explanation becomes the final assistant output and the turn is refused with category blocked. Do not use this for minor assumptions, partial-but-useful answers, recoverable failures, or work you merely prefer not to do. Call this tool alone, with no assistant text or sibling tools. If the call is reported as not run, correct the violation and issue a fresh standalone report_blocked call.",
    )
}

/// Foreground-only model tool contract for delegating one bounded task.
///
/// The worker derives the durable child identity from the model tool call and
/// atomically checkpoints the result before continuing the foreground turn.
/// Sandboxed agents never receive this definition, so the v1 hierarchy cannot
/// recurse past depth one.
#[must_use]
pub fn spawn_sandbox_agent_tool_spec() -> ToolSpec {
    ToolSpec::for_args::<SpawnSandboxAgentArgs>(
        SPAWN_SANDBOX_AGENT_TOOL,
        "Delegate one self-contained task to an isolated background agent and continue immediately. Save every returned agent_id. Call this tool alone, with no assistant text or sibling tools, including other spawn_sandbox_agent calls. Before final completion, include every spawned agent_id in a standalone wait_for_agents call, batching independent IDs into one wait. Do not ask a child to spawn more agents. If a spawn is reported as not run, correct the reported violation and issue a fresh standalone spawn call.",
    )
}

/// Prepared foreground-only contract for waiting on depth-one children.
///
/// It is advertised only with the matching non-blocking spawn contract from a
/// durably claimed foreground turn.
#[must_use]
pub fn wait_for_agents_tool_spec() -> ToolSpec {
    ToolSpec::for_args::<WaitForAgentsArgs>(
        WAIT_FOR_AGENTS_TOOL,
        "Wait until all specified background agents finish, then return their results in the same order. Use only depth-one agent IDs returned by spawn_sandbox_agent. Call this tool alone, with no assistant text or sibling tools. If the wait is reported as not run, follow the result's correction and issue a fresh standalone wait call.",
    )
}

/// Normalize one exact web-search hostname, rejecting URLs and wildcard syntax.
///
/// Search-domain filters are deliberately narrower than general URL hosts:
/// they are ASCII DNS names (or canonical dotted-decimal IPv4 literals), with
/// no scheme, path, port, credentials, wildcard, or control characters. A
/// trailing root dot and surrounding whitespace are accepted and removed so
/// the host-owned parser preserves its existing user-facing normalization.
#[must_use]
pub fn canonical_web_search_domain(value: &str) -> Option<String> {
    let normalized = value.trim().trim_end_matches('.').to_ascii_lowercase();
    if normalized.is_empty()
        || normalized.chars().count() > MAX_WEB_SEARCH_DOMAIN_CHARS
        || !normalized.is_ascii()
    {
        return None;
    }

    let labels = normalized.split('.').collect::<Vec<_>>();
    if labels.iter().any(|label| {
        label.is_empty()
            || label.len() > 63
            || !label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            || !label
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphanumeric)
            || !label
                .as_bytes()
                .last()
                .is_some_and(u8::is_ascii_alphanumeric)
    }) {
        return None;
    }

    if labels
        .last()
        .is_some_and(|label| label.bytes().all(|byte| byte.is_ascii_digit()))
    {
        let address = normalized.parse::<std::net::Ipv4Addr>().ok()?;
        if address.to_string() != normalized {
            return None;
        }
    }

    Some(normalized)
}

/// Whether one provider-executed search receipt is safe to persist and replay
/// as Tidebreak's canonical [`WEB_SEARCH_TOOL`] call.
///
/// Provider adapters are a normalization boundary, but their input is still a
/// remote provider response. Admission therefore validates the complete final
/// receipt before the agent creates a transcript block, activity event, or
/// durable row. Adjacent native actions (`open_page`, `find_in_page`), partial
/// or over-budget result bodies, and opaque replay that is not a bounded search
/// pair remain transient provider state and are excluded without failing the
/// turn.
#[must_use]
pub fn provider_web_search_receipt_is_canonical(
    input: &Value,
    output: &Value,
    is_error: bool,
    replay: Option<&crate::provider::ProviderToolReplay>,
) -> bool {
    provider_web_search_input_is_canonical(input)
        && if is_error {
            provider_web_search_error_is_canonical(output)
        } else {
            provider_web_search_output_is_canonical(output)
        }
        && replay.is_none_or(|replay| provider_web_search_replay_is_canonical(replay, input))
}

fn provider_web_search_input_is_canonical(input: &Value) -> bool {
    let Ok(arguments) = serde_json::from_value::<WebSearchArgs>(input.clone()) else {
        return false;
    };
    serde_json::to_vec(input).is_ok_and(|encoded| {
        encoded.len() <= ToolCallRecord::MAX_ARGUMENT_BYTES && !encoded.contains(&0)
    }) && input.get("summary").is_none_or(|summary| {
        summary.as_str().is_some_and(|summary| {
            summary.chars().count() <= crate::preview::MAX_ACTION_SUMMARY_CHARS
                && !summary.contains('\0')
        })
    }) && ["start_published_at", "end_published_at"]
        .into_iter()
        .all(|name| input.get(name).is_none_or(Value::is_string))
        && !arguments.query.trim().is_empty()
        && arguments.query == arguments.query.trim()
        && arguments.query.chars().count() <= MAX_WEB_SEARCH_QUERY_CHARS
        && !arguments.query.chars().any(char::is_control)
        && (1..=MAX_WEB_SEARCH_RESULTS).contains(&arguments.max_results)
        && arguments.domains.len() <= MAX_WEB_SEARCH_DOMAINS
        && arguments
            .domains
            .iter()
            .all(|domain| canonical_web_search_domain(domain).is_some())
        && arguments
            .start_published_at
            .is_none_or(|start| arguments.end_published_at.is_none_or(|end| start <= end))
}

fn provider_web_search_output_is_canonical(output: &Value) -> bool {
    if !serialized_value_fits(output, MAX_PROVIDER_WEB_SEARCH_OUTPUT_BYTES) {
        return false;
    }
    let Some(object) = output.as_object() else {
        return false;
    };
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "provider" | "results"))
    {
        return false;
    }
    let Some(provider) = object.get("provider").and_then(Value::as_str) else {
        return false;
    };
    if !bounded_safe_label(provider, ToolCallRecord::MAX_LABEL_LEN) {
        return false;
    }
    let Some(results) = object.get("results").and_then(Value::as_array) else {
        return false;
    };
    if results.len() > MAX_WEB_SEARCH_RESULTS {
        return false;
    }

    let mut urls = HashSet::with_capacity(results.len());
    results.iter().all(|result| {
        provider_web_search_result_is_canonical(result)
            && urls.insert(result["url"].as_str().unwrap_or_default())
    })
}

fn provider_web_search_result_is_canonical(result: &Value) -> bool {
    let Some(object) = result.as_object() else {
        return false;
    };
    if object.keys().any(|key| {
        !matches!(
            key.as_str(),
            "url"
                | "title"
                | "snippet"
                | "content"
                | "score"
                | "published_at"
                | "image_url"
                | "metadata"
        )
    }) {
        return false;
    }
    let (Some(url), Some(title), Some(snippet)) = (
        object.get("url").and_then(Value::as_str),
        object.get("title").and_then(Value::as_str),
        object.get("snippet").and_then(Value::as_str),
    ) else {
        return false;
    };
    if !provider_result_url_is_canonical(url)
        || title.chars().count() > MAX_PROVIDER_WEB_SEARCH_TITLE_CHARS
        || snippet.chars().count() > MAX_PROVIDER_WEB_SEARCH_SNIPPET_CHARS
        || title.contains('\0')
        || snippet.contains('\0')
        || (title.is_empty() && snippet.is_empty())
    {
        return false;
    }
    if object.get("content").is_some_and(|content| {
        content.as_str().is_none_or(|content| {
            content.chars().count() > MAX_PROVIDER_WEB_SEARCH_CONTENT_CHARS
                || content.contains('\0')
        })
    }) {
        return false;
    }
    if object
        .get("score")
        .is_some_and(|score| score.as_f64().is_none_or(|score| !score.is_finite()))
    {
        return false;
    }
    if object.get("published_at").is_some_and(|published_at| {
        published_at
            .as_str()
            .is_none_or(|published_at| published_at.parse::<DateTime<Utc>>().is_err())
    }) {
        return false;
    }
    if object.get("image_url").is_some_and(|image_url| {
        image_url
            .as_str()
            .is_none_or(|image_url| !provider_result_url_is_canonical(image_url))
    }) {
        return false;
    }
    if object
        .get("metadata")
        .is_some_and(|metadata| !provider_web_search_metadata_is_canonical(metadata))
    {
        return false;
    }
    true
}

fn provider_web_search_metadata_is_canonical(metadata: &Value) -> bool {
    let Some(metadata) = metadata.as_object() else {
        return false;
    };
    metadata.len() <= MAX_PROVIDER_WEB_SEARCH_METADATA_ENTRIES
        && metadata.iter().all(|(key, value)| {
            !key.is_empty()
                && key.chars().count() <= MAX_PROVIDER_WEB_SEARCH_METADATA_KEY_CHARS
                && !key.contains('\0')
                && value.as_str().is_some_and(|value| {
                    value.chars().count() <= MAX_PROVIDER_WEB_SEARCH_METADATA_VALUE_CHARS
                        && !value.contains('\0')
                })
        })
}

fn provider_web_search_error_is_canonical(output: &Value) -> bool {
    if !serialized_value_fits(output, MAX_PROVIDER_WEB_SEARCH_OUTPUT_BYTES) {
        return false;
    }
    let Some(object) = output.as_object() else {
        return false;
    };
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "error_code" | "error_detail"))
    {
        return false;
    }
    let Some(error_code) = object.get("error_code").and_then(Value::as_str) else {
        return false;
    };
    bounded_safe_label(error_code, ToolCallRecord::MAX_ERROR_CODE_LEN)
        && object.get("error_detail").is_none_or(|detail| {
            detail.as_str().is_some_and(|detail| {
                !detail.is_empty()
                    && detail.len() <= ToolCallRecord::MAX_ERROR_DETAIL_LEN
                    && !detail.chars().any(char::is_control)
            })
        })
}

fn provider_web_search_replay_is_canonical(
    replay: &crate::provider::ProviderToolReplay,
    input: &Value,
) -> bool {
    if replay.is_empty()
        || !serialized_value_fits(
            &serde_json::to_value(replay).unwrap_or(Value::Null),
            MAX_PROVIDER_WEB_SEARCH_REPLAY_BYTES,
        )
    {
        return false;
    }
    let Some(origin) = replay.origin() else {
        return false;
    };
    if !bounded_safe_label(&origin.model, ToolCallRecord::MAX_LABEL_LEN)
        || origin
            .provider
            .as_ref()
            .is_some_and(|provider| !bounded_safe_label(&provider.0, ToolCallRecord::MAX_LABEL_LEN))
        || replay.blocks().len() > MAX_PROVIDER_WEB_SEARCH_REPLAY_BLOCKS
    {
        return false;
    }
    if replay.blocks().is_empty() {
        return true;
    }

    // Anthropic is currently the only adapter with opaque native search
    // replay. Its replay is one exact server-tool/result pair. Accepting an
    // arbitrary provider-native block here would let a hostile response smuggle
    // a different operation into the next same-route request even though the
    // cleartext receipt was admitted as `web_search`.
    let [call, result] = replay.blocks() else {
        return false;
    };
    let (Some(call), Some(result)) = (call.as_object(), result.as_object()) else {
        return false;
    };
    let Some(call_id) = call.get("id").and_then(Value::as_str) else {
        return false;
    };
    bounded_safe_label(call_id, ToolCallRecord::MAX_LABEL_LEN)
        && call.get("type").and_then(Value::as_str) == Some("server_tool_use")
        && call.get("name").and_then(Value::as_str) == Some(WEB_SEARCH_TOOL)
        && call.get("input") == Some(input)
        && result.get("type").and_then(Value::as_str) == Some("web_search_tool_result")
        && result.get("tool_use_id").and_then(Value::as_str) == Some(call_id)
        && result.contains_key("content")
}

fn serialized_value_fits(value: &Value, max_bytes: usize) -> bool {
    serde_json::to_vec(value).is_ok_and(|encoded| encoded.len() <= max_bytes)
}

fn bounded_safe_label(value: &str, max_bytes: usize) -> bool {
    !value.is_empty() && value.len() <= max_bytes && !value.chars().any(char::is_control)
}

/// Validate one already-normalized HTTP(S) result URL without widening core's
/// dependency surface. Provider adapters must emit a network URL, not a native
/// action target or display string: scheme and host are required, fragments
/// and whitespace/control characters are excluded, and the serialized URL is
/// held to the host search normalizer's 2,048-byte limit.
fn provider_result_url_is_canonical(value: &str) -> bool {
    if value.is_empty()
        || value.len() > MAX_WEB_EXTRACT_URL_BYTES
        || value.contains('#')
        || value.chars().any(|character| {
            character.is_control()
                || character.is_whitespace()
                || matches!(
                    character,
                    '\u{200b}'..='\u{200f}'
                        | '\u{202a}'..='\u{202e}'
                        | '\u{2066}'..='\u{2069}'
                        | '\u{feff}'
                )
        })
    {
        return false;
    }
    let Some(rest) = value
        .strip_prefix("https://")
        .or_else(|| value.strip_prefix("http://"))
    else {
        return false;
    };
    let authority_end = rest
        .find(|character| ['/', '?'].contains(&character))
        .unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    if authority.is_empty() || !authority.is_ascii() {
        return false;
    }
    let host_port = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    if host_port.is_empty() {
        return false;
    }
    if let Some(ipv6) = host_port.strip_prefix('[') {
        let Some((address, port)) = ipv6.split_once(']') else {
            return false;
        };
        return !address.is_empty()
            && address
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() || matches!(byte, b':' | b'.'))
            && (port.is_empty()
                || port
                    .strip_prefix(':')
                    .is_some_and(|port| valid_url_port(Some(port))));
    }
    if host_port.bytes().filter(|byte| *byte == b':').count() > 1 {
        return false;
    }
    let (host, port) = host_port
        .rsplit_once(':')
        .map_or((host_port, None), |(host, port)| (host, Some(port)));
    !host.is_empty()
        && host == host.to_ascii_lowercase()
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                && label
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && label
                    .as_bytes()
                    .last()
                    .is_some_and(u8::is_ascii_alphanumeric)
        })
        && valid_url_port(port)
}

fn valid_url_port(port: Option<&str>) -> bool {
    port.is_none_or(|port| {
        !port.is_empty()
            && port.bytes().all(|byte| byte.is_ascii_digit())
            && port.parse::<u16>().is_ok()
    })
}

/// Narrow, host-executed web-search contract shared by trusted runtimes.
///
/// The provider-backed implementation belongs to `tidebreak-server::web_search`; core
/// owns this schema because the sandbox checkpoint protocol must advertise the
/// same closed arguments without depending on a network integration crate.
#[must_use]
pub fn web_search_tool_spec() -> ToolSpec {
    ToolSpec::for_args::<WebSearchArgs>(
        WEB_SEARCH_TOOL,
        "Search the public web for current information. Use focused queries and cite sources with the exact result URLs. Results may be unavailable when the host has not configured web search.",
    )
}

/// Host-executed contract for reading one exact public web page.
///
/// The extraction implementation belongs to `tidebreak-server::web_search`; core owns
/// this schema beside the shared web-search contract so every trusted runtime
/// advertises the same closed arguments.
#[must_use]
pub fn web_extract_tool_spec() -> ToolSpec {
    ToolSpec::for_args::<WebExtractArgs>(
        WEB_EXTRACT_TOOL,
        "Fetch one exact public web page URL and return its readable content. Use it to open a page found through search and verify claims against the page itself instead of relying on snippets; cite the exact page URL.",
    )
}

/// Sandbox-specific presentation of the shared web-search contract.
///
/// The checkpoint worker, not the ordinary tool registry, executes this
/// definition under its own durable lease, which is why the sandbox needs its
/// own wording at all.
#[must_use]
pub fn sandbox_web_search_tool_spec() -> ToolSpec {
    let mut spec = web_search_tool_spec();
    spec.description = "Search the public web for current information. Use focused queries and cite sources with the exact result URLs. Results may be unavailable when the host has not configured web search.".into();
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
    ToolSpec::for_args::<SandboxReadDelegatedFileArgs>(
        SANDBOX_READ_DELEGATED_FILE_TOOL,
        "Read the exact UTF-8 file delegated to this background task.",
    )
}

/// Whether arguments are the one canonical argument-free payload.
#[must_use]
pub fn validate_sandbox_read_delegated_file_arguments(arguments: &Value) -> bool {
    serde_json::from_value::<SandboxReadDelegatedFileArgs>(arguments.clone()).is_ok()
}

/// Contract for running one command in a background run's private workspace.
///
/// The description is where a background agent learns the only way it has to
/// produce something the user can keep: write the file under `output/`. The
/// filename becomes the output's name, so the agent names its own deliverables
/// and the host never invents a title for it.
#[must_use]
pub fn sandbox_exec_tool_spec() -> ToolSpec {
    ToolSpec::for_args::<SandboxExecArgs>(
        SANDBOX_EXEC_TOOL,
        "Run one executable with an argument vector in this background task's own private \
         workspace. command is argv[0] only — a single binary or path with no spaces (python3, \
         mkdir, /bin/sh). Put every other token in args: mkdir -p output is command mkdir with \
         args [\"-p\", \"output\"]; a shell one-liner is command /bin/sh with args [\"-c\", \"…\"]. \
         No shell parses the arguments unless you invoke a shell that way. The workspace starts \
         empty and is yours alone: it holds nothing but what your own earlier commands wrote, and \
         you cannot reach the conversation, the user's files, or any connected folder from it. \
         Write only under the workspace (relative paths); absolute paths like /tmp are not durable \
         here. Save only final deliverables under output/ (for example .md, .csv, .py, .tsx, \
         .xlsx, .pdf, .pptx, .docx, .chart.json) — each file there is published to the user as a durable output \
         named by its own filename, and writing the same filename again publishes a new version of \
         that same output. Keep helper scripts and other intermediates in the workspace root (or \
         any path outside output/). Source files are valid final deliverables when the task asks for \
         code. Name deliverables the way you want the user to see them. Every command returns bounded \
         stdout and stderr.",
    )
}

/// Whether arguments are a bounded, canonical sandbox exec payload.
#[must_use]
pub fn validate_sandbox_exec_arguments(arguments: &Value) -> bool {
    serde_json::from_value::<SandboxExecArgs>(arguments.clone())
        .is_ok_and(|arguments| arguments.is_well_formed())
}

/// Contract by which a background run finishes and submits its own files.
///
/// This is the only way a background run says what it produced. There is no
/// host-side synthesis of a result document: if the run wants the user to keep
/// something, it writes the file and names it here.
#[must_use]
pub fn sandbox_done_tool_spec() -> ToolSpec {
    ToolSpec::for_args::<SandboxDoneArgs>(
        SANDBOX_DONE_TOOL,
        "Finish this background task. List the filenames of final deliverables you wrote under \
         output/ — not helper scripts — that answer the task; those files are what the user \
         receives, named exactly as you named them. Give a short summary of what you produced. \
         Call this only after the files exist. If the task genuinely produced no file, submit no \
         filenames and say so in the summary.",
    )
}

/// Whether arguments are a bounded, canonical sandbox submission payload.
#[must_use]
pub fn validate_sandbox_done_arguments(arguments: &Value) -> bool {
    serde_json::from_value::<SandboxDoneArgs>(arguments.clone())
        .is_ok_and(|arguments| arguments.is_well_formed())
}

impl SandboxExecArgs {
    /// Whether every field is within the advertised bounds.
    ///
    /// Schema bounds are advisory — a provider may forward anything — so the
    /// durable checkpoint admission and the executor both check them here. The
    /// working directory is only bounded in length; the provider owns the
    /// traversal rules for its own filesystem.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        !self.command.is_empty()
            && self.command.len() <= MAX_SANDBOX_EXEC_COMMAND_BYTES
            && self.args.len() <= MAX_SANDBOX_EXEC_ARGUMENTS
            && !self.cwd.is_empty()
            && self.cwd.len() <= MAX_SANDBOX_EXEC_CWD_BYTES
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn child_ids(count: usize) -> Vec<AgentRunId> {
        (0..count).map(|_| AgentRunId::new()).collect()
    }

    fn inbox(agent_id: AgentRunId, text: String) -> AgentRunInboxEntry {
        inbox_payload(
            agent_id,
            AgentRunResultPayload::FinalText { text: text.clone() },
            text,
        )
    }

    fn inbox_payload(
        agent_id: AgentRunId,
        payload: AgentRunResultPayload,
        text: String,
    ) -> AgentRunInboxEntry {
        let now = Utc::now();
        AgentRunInboxEntry {
            parent_run_id: AgentRunId::new(),
            child_run_id: agent_id,
            chat_id: crate::SessionId::new(),
            result: crate::AgentRunResult {
                agent_run_id: agent_id,
                lease_token: uuid::Uuid::new_v4(),
                attempt_count: 1,
                claim_count: 1,
                payload,
                text,
                model_steps: 0,
                usage: crate::provider::Usage::default(),
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
        assert!(spec.input_schema.get("title").is_none());
        assert!(spec.input_schema.get("$schema").is_none());
        assert_eq!(spec.input_schema["additionalProperties"], false);
        assert_eq!(spec.input_schema["required"], serde_json::json!(["task"]));
        assert_eq!(spec.input_schema["properties"]["task"]["maxLength"], 16_000);
        assert_eq!(
            spec.input_schema["properties"]["resource"]["required"],
            serde_json::json!(["root_id", "relative_path"])
        );
        assert_eq!(
            spec.input_schema["properties"]["resource"]["properties"]["root_id"],
            serde_json::json!({"type": "string", "format": "uuid"})
        );
        assert!(spec.description.contains("Do not ask a child to spawn"));
        assert!(spec.description.contains("Call this tool alone"));
        assert!(spec
            .description
            .contains("including other spawn_sandbox_agent calls"));
        assert!(spec.description.contains("reported as not run"));
        assert!(spec.description.contains("fresh standalone spawn call"));
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
        assert_eq!(
            spec.input_schema["properties"]["agent_ids"]["items"],
            serde_json::json!({"type": "string", "format": "uuid"})
        );
        assert!(spec.description.contains("all specified"));
        assert!(spec.description.contains("same order"));
        assert!(spec.description.contains("Call this tool alone"));
        assert!(spec.description.contains("fresh standalone wait call"));
    }

    #[test]
    fn report_blocked_contract_is_closed_bounded_and_machine_readable() {
        let valid = serde_json::json!({
            "reason_code": "required_source_missing",
            "explanation": "I cannot produce the requested report because the required source is unavailable."
        });
        let parsed = parse_report_blocked_arguments(&valid).unwrap();
        assert_eq!(parsed.reason_code, "required_source_missing");
        assert_eq!(
            parsed.explanation,
            "I cannot produce the requested report because the required source is unavailable."
        );

        for invalid in [
            serde_json::json!({}),
            serde_json::json!({"reason_code": "UPPER", "explanation": "Blocked."}),
            serde_json::json!({"reason_code": "bad-hyphen", "explanation": "Blocked."}),
            serde_json::json!({"reason_code": "missing_source", "explanation": ""}),
            serde_json::json!({"reason_code": "missing_source", "explanation": " padded "}),
            serde_json::json!({"reason_code": "missing_source", "explanation": "bad\u{0}"}),
            serde_json::json!({
                "reason_code": "missing_source",
                "explanation": "x".repeat(MAX_REPORT_BLOCKED_EXPLANATION_CHARS + 1)
            }),
            serde_json::json!({
                "reason_code": "x".repeat(MAX_REPORT_BLOCKED_REASON_CODE_CHARS + 1),
                "explanation": "Blocked."
            }),
            serde_json::json!({
                "reason_code": "missing_source",
                "explanation": "Blocked.",
                "retry": false
            }),
        ] {
            assert!(!validate_report_blocked_arguments(&invalid), "{invalid}");
        }
    }

    #[test]
    fn report_blocked_spec_makes_terminal_and_standalone_semantics_explicit() {
        let spec = report_blocked_tool_spec();

        assert_eq!(spec.name, REPORT_BLOCKED_TOOL);
        assert_eq!(spec.input_schema["additionalProperties"], false);
        assert_eq!(
            spec.input_schema["required"],
            serde_json::json!(["reason_code", "explanation"])
        );
        assert_eq!(
            spec.input_schema["properties"]["reason_code"]["maxLength"],
            MAX_REPORT_BLOCKED_REASON_CODE_CHARS
        );
        assert_eq!(
            spec.input_schema["properties"]["explanation"]["maxLength"],
            MAX_REPORT_BLOCKED_EXPLANATION_CHARS
        );
        assert!(spec.description.contains("final assistant output"));
        assert!(spec.description.contains("refused with category blocked"));
        assert!(spec.description.contains("Call this tool alone"));
        assert!(spec
            .description
            .contains("fresh standalone report_blocked call"));
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
        // The narration is required of the model too: a card that leads with
        // a sentence only when the model felt like writing one is a card that
        // reads differently call to call.
        assert_eq!(
            foreground.input_schema["required"],
            serde_json::json!(["summary", "query"])
        );
        assert_eq!(
            foreground.input_schema["properties"]["start_published_at"],
            serde_json::json!({"type": "string", "format": "date-time"})
        );
        assert_eq!(
            foreground.input_schema["properties"]["end_published_at"],
            serde_json::json!({"type": "string", "format": "date-time"})
        );
        // Omitting `max_results` is a choice, so the model is told what it
        // chooses by omitting it.
        assert_eq!(
            foreground.input_schema["properties"]["max_results"]["default"],
            DEFAULT_WEB_SEARCH_RESULTS
        );
        assert_eq!(foreground.input_schema["additionalProperties"], false);
        assert!(foreground.description.contains("exact result URLs"));
        assert!(sandbox.description.contains("exact result URLs"));
    }

    #[test]
    fn only_host_shaped_provider_search_inputs_are_canonical() {
        assert!(provider_web_search_input_is_canonical(&serde_json::json!({
            "summary": "Checking current Tidebreak coverage",
            "query": "Tidebreak agent tools",
            "max_results": 3,
            "domains": [" Docs.Example-Services.com. "],
        })));

        assert_eq!(
            canonical_web_search_domain(" Docs.Example-Services.com. "),
            Some("docs.example-services.com".to_owned())
        );

        for invalid_domain in [
            "*.example.com".to_owned(),
            "https://example.com".to_owned(),
            "example.com/path".to_owned(),
            "example.com:443".to_owned(),
            "a".repeat(MAX_WEB_SEARCH_DOMAIN_CHARS + 1),
            "exa\u{1}mple.com".to_owned(),
        ] {
            let invalid = serde_json::json!({
                "summary": "Checking current Tidebreak coverage",
                "query": "Tidebreak agent tools",
                "domains": [invalid_domain],
            });
            assert!(
                !provider_web_search_input_is_canonical(&invalid),
                "{invalid}"
            );
        }

        for native_or_invalid in [
            serde_json::json!({}),
            serde_json::json!({
                "query": {"type": "open_page", "url": "https://example.com"}
            }),
            serde_json::json!({
                "query": {"type": "find_in_page", "pattern": "Tidebreak"}
            }),
            serde_json::json!({"query": " leading whitespace"}),
            serde_json::json!({"query": "control\u{1}"}),
            serde_json::json!({"summary": null, "query": "search"}),
            serde_json::json!({"summary": "x".repeat(201), "query": "search"}),
            serde_json::json!({"query": "search", "max_results": 0}),
            serde_json::json!({"query": "search", "start_published_at": null}),
            serde_json::json!({
                "query": "search",
                "start_published_at": "2026-08-14T00:00:01Z",
                "end_published_at": "2026-08-14T00:00:00Z",
            }),
        ] {
            assert!(
                !provider_web_search_input_is_canonical(&native_or_invalid),
                "{native_or_invalid}"
            );
        }
    }

    #[test]
    fn complete_provider_search_receipts_are_bounded_before_replay() {
        let input = serde_json::json!({"query": "Tidebreak release notes"});
        let output = serde_json::json!({
            "provider": "anthropic",
            "results": [{
                "url": "https://example.com/notes",
                "title": "Release notes",
                "snippet": "What shipped",
                "metadata": {"page_age": "August 14, 2026"},
            }],
        });
        assert!(provider_web_search_receipt_is_canonical(
            &input, &output, false, None
        ));

        let origin = crate::provider::ReasoningOrigin {
            provider: Some(crate::provider::ProviderId::new("anthropic")),
            model: "claude-test".into(),
        };
        let replay = crate::provider::ProviderToolReplay::captured(
            origin.clone(),
            vec![
                serde_json::json!({
                    "type": "server_tool_use",
                    "id": "srvtoolu_1",
                    "name": WEB_SEARCH_TOOL,
                    "input": input,
                }),
                serde_json::json!({
                    "type": "web_search_tool_result",
                    "tool_use_id": "srvtoolu_1",
                    "content": [{"encrypted_content": "opaque"}],
                }),
            ],
        );
        assert!(provider_web_search_receipt_is_canonical(
            &input,
            &output,
            false,
            Some(&replay)
        ));

        let oversized_error = serde_json::json!({
            "error_code": "x".repeat(ToolCallRecord::MAX_ERROR_CODE_LEN + 1),
        });
        assert!(!provider_web_search_receipt_is_canonical(
            &input,
            &oversized_error,
            true,
            None,
        ));
        assert!(provider_web_search_receipt_is_canonical(
            &input,
            &serde_json::json!({"error_code": "max_uses_exceeded"}),
            true,
            None,
        ));

        let wrong_replay = crate::provider::ProviderToolReplay::captured(
            origin,
            vec![
                serde_json::json!({
                    "type": "server_tool_use",
                    "id": "srvtoolu_1",
                    "name": "exec",
                    "input": input,
                }),
                serde_json::json!({
                    "type": "web_search_tool_result",
                    "tool_use_id": "srvtoolu_1",
                    "content": [],
                }),
            ],
        );
        assert!(!provider_web_search_receipt_is_canonical(
            &input,
            &output,
            false,
            Some(&wrong_replay),
        ));

        let oversized_replay = crate::provider::ProviderToolReplay::captured(
            crate::provider::ReasoningOrigin {
                provider: Some(crate::provider::ProviderId::new("anthropic")),
                model: "claude-test".into(),
            },
            vec![
                serde_json::json!({
                    "type": "server_tool_use",
                    "id": "srvtoolu_1",
                    "name": WEB_SEARCH_TOOL,
                    "input": input,
                }),
                serde_json::json!({
                    "type": "web_search_tool_result",
                    "tool_use_id": "srvtoolu_1",
                    "content": [{
                        "encrypted_content": "x".repeat(MAX_PROVIDER_WEB_SEARCH_REPLAY_BYTES),
                    }],
                }),
            ],
        );
        assert!(!provider_web_search_receipt_is_canonical(
            &input,
            &output,
            false,
            Some(&oversized_replay),
        ));
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

    #[test]
    fn wait_result_projection_shortens_large_final_text_and_keeps_child_outputs() {
        let essay_id = AgentRunId::new();
        let submit_id = AgentRunId::new();
        let essay = "write-up ".repeat(8_000);
        let original_essay = AgentRunResultPayload::FinalText {
            text: essay.clone(),
        };
        let outputs = vec![crate::AgentRunSubmittedOutput {
            output_id: crate::OutputId::new(),
            filename: "briefing.md".into(),
        }];
        let original_submission = AgentRunResultPayload::Submission {
            outputs: outputs.clone(),
            summary: "Wrote the briefing.".into(),
        };
        let entries = vec![
            inbox_payload(essay_id, original_essay.clone(), essay.clone()),
            inbox_payload(
                submit_id,
                original_submission.clone(),
                "Wrote the briefing.".into(),
            ),
        ];

        let encoded = canonical_wait_for_agents_result(&entries).unwrap();
        assert!(encoded.len() <= ToolCallRecord::MAX_RESULT_BYTES);
        assert_eq!(entries[0].result.payload, original_essay);
        assert_eq!(entries[1].result.payload, original_submission);

        let value: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        let results = value["results"].as_array().unwrap();
        assert_eq!(results.len(), 2);

        assert_eq!(results[0]["agent_id"], essay_id.to_string());
        assert_eq!(results[0]["truncated"], true);
        let projected = results[0]["result"]["text"].as_str().unwrap();
        assert!(projected.ends_with(WAIT_RESULT_TRUNCATION_MARKER));
        assert!(projected.len() < essay.len());
        assert!(
            serde_json::to_vec(&results[0]).unwrap().len() <= MAX_WAIT_FOR_AGENT_RESULT_JSON_BYTES
        );

        assert_eq!(results[1]["agent_id"], submit_id.to_string());
        assert_eq!(results[1]["result"]["kind"], "submission");
        assert_eq!(
            results[1]["result"]["outputs"][0]["filename"],
            "briefing.md"
        );
        assert_eq!(
            results[1]["result"]["outputs"][0]["output_id"],
            outputs[0].output_id.to_string()
        );
    }
}
