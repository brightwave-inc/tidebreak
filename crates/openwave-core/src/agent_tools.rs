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
/// Maximum host-executed *work* checkpoints one sandbox run may accumulate.
///
/// A sandbox run's checkpoints form a chain the worker replays in full on every
/// claim, so the count is bounded rather than open-ended: the whole chain rides
/// each model request. Real delegated work needs a sequence — search, read,
/// then several commands to produce a file — so the bound is a working budget,
/// not the one-call fence it replaced.
///
/// `update_task_plan` rows are counted against
/// [`MAX_SANDBOX_TASK_PLAN_CALLS`] instead, so the run's bookkeeping cannot
/// spend the allowance its actual work needs.
pub const MAX_SANDBOX_TOOL_CALLS: usize = 16;
/// Maximum `update_task_plan` checkpoints one sandbox run may accumulate.
///
/// Plan rows are budgeted separately from the work budget above, and the reason
/// is the prompt: a run is told to keep its plan current as steps finish, which
/// is a call after most real steps. Charged to the same 16 rows, bookkeeping
/// would starve the exec and search calls the task is actually for — the run
/// would run out of budget describing work it never got to do. Their own cap
/// keeps that from happening while still bounding a model that does nothing but
/// rewrite its checklist. It is smaller than the work budget because a plan is
/// replaced whole: eight revisions is a generous account of one delegated task.
pub const MAX_SANDBOX_TASK_PLAN_CALLS: usize = 8;
/// Maximum tool calls one model step may park as a single batch.
///
/// A step's calls are parked together and replayed together, so this bounds how
/// much a single completion can commit against the run's total budget above.
pub const MAX_SANDBOX_TOOL_CALLS_PER_STEP: usize = 8;

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
/// Default result count when a model omits `max_results`.
pub const DEFAULT_WEB_SEARCH_RESULTS: usize = 5;
/// Maximum executable length advertised to a sandboxed background agent.
///
/// The three sandbox-exec bounds mirror the ones `openwave-code-execution`
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
/// `openwave-web-search` holds its fetch-admission URL bound to this value, so
/// the schema a model sees and the policy the fetcher enforces cannot drift.
pub const MAX_WEB_EXTRACT_URL_BYTES: usize = 2_048;

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
    /// Executable name or path.
    #[schemars(
        length(min = 1, max = MAX_SANDBOX_EXEC_COMMAND_BYTES),
        description = "Executable name or path."
    )]
    pub command: String,
    /// Arguments passed directly to the executable, with no shell parsing.
    #[serde(default)]
    #[schemars(
        length(max = MAX_SANDBOX_EXEC_ARGUMENTS),
        description = "Arguments passed directly to the executable."
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
    ToolSpec::for_args::<SpawnSandboxAgentArgs>(
        SPAWN_SANDBOX_AGENT_TOOL,
        "Delegate one self-contained task to an isolated background agent and continue immediately. Save every returned agent_id. Before final completion, include every spawned agent_id in a wait_for_agents call; batch independent IDs into one wait, and do not ask it to spawn more agents.",
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
        "Wait until all specified background agents finish, then return their results in the same order. Use only depth-one agent IDs returned by spawn_sandbox_agent.",
    )
}

/// Narrow, host-executed web-search contract shared by trusted runtimes.
///
/// The provider-backed implementation belongs to `openwave-web-search`; core
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
/// The extraction implementation belongs to `openwave-web-search`; core owns
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
         workspace. No shell parses the arguments unless you invoke a shell explicitly (for \
         example command '/bin/sh' with args ['-c', '…']). The workspace starts empty and is \
         yours alone: it holds nothing but what your own earlier commands wrote, and you cannot \
         reach the conversation, the user's files, or any connected folder from it. Save every \
         deliverable under output/ — each file you write there is published to the user as a \
         durable output named by its own filename, and writing the same filename again publishes \
         a new version of that same output. Name those files the way you want the user to see \
         them. Every command returns bounded stdout and stderr.",
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
        "Finish this background task. List the filenames you wrote under output/ that are the \
         deliverables the task asked for — those files are what the user receives, named exactly \
         as you named them — and give a short summary of what you produced. Call this only after \
         the files exist. If the task genuinely produced no file, submit no filenames and say so \
         in the summary.",
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
        assert_eq!(
            spec.input_schema["properties"]["agent_ids"]["items"],
            serde_json::json!({"type": "string", "format": "uuid"})
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
        assert_eq!(
            foreground.input_schema["required"],
            serde_json::json!(["query"])
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
