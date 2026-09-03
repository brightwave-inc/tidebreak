//! Sandbox agent-run worker configuration and system prompt.

use std::path::PathBuf;
#[cfg(test)]
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::Arc;
use std::time::Duration;

use tidebreak_code_execution::{PluginPackage, SkillPackage};
use tidebreak_core::{AgentConfig, SecretProvider, Store, TurnWebSearch};
use tokio::sync::Notify;

use crate::bus::EventBus;
use crate::code_execution::ConfiguredExecProvider;
use crate::foreground_prompt::skill_summary_catalog_lines;
use crate::lane::{LaneOutcome, LaneStep};
use crate::resolver::ProviderResolver;
use crate::retry::RetrySchedule;
use crate::state::SandboxAttemptGuard;

/// Fixed instruction set for the initial isolated executor, up to the sentence
/// that names the run's tools.
///
/// Deliberately do not inherit the foreground system prompt: it may describe
/// interactive tools or conversation-wide responsibilities that are outside a
/// depth-one child run's authority.
pub(super) const SANDBOX_PROMPT_PREAMBLE: &str = "You are a sandboxed background agent. Work only on the delegated task below. You cannot access the conversation, the user's files, connected folders, or other agents. You do have your own private workspace: use exec to run commands in it. When the task asks for a file, write each requested deliverable under output/ — each file there is published to the user as a durable output named by its own filename. When the task asks for plain text, return the full text directly and do not create a file instead.";
pub(super) const SANDBOX_DELEGATED_FILE_PROMPT_PREAMBLE: &str = "You are a sandboxed background agent. Work only on the delegated task below. You cannot access the conversation, the user's files, connected folders, or other agents, except for the one exact file explicitly delegated to this run. You do have your own private workspace: use exec to run commands in it. When the task asks for a file, write each requested deliverable under output/ — each file there is published to the user as a durable output named by its own filename. When the task asks for plain text, return the full text directly and do not create a file instead.";
/// How to form exec calls. Stress-tested agents routinely waste steps by
/// stuffing a whole shell line into `command` (which is argv[0] only) or by
/// writing under `/tmp`, which this workspace cannot keep or publish.
pub(super) const SANDBOX_PROMPT_EXEC_CLAUSE: &str = "exec with argv form only — command is a single executable (for example python3, mkdir, /bin/sh), and each shell token is its own args entry (mkdir -p output is command mkdir with args [\"-p\", \"output\"]; a one-liner is command /bin/sh with args [\"-c\", \"…\"]). Never put spaces inside command. Stay inside the workspace: create files under output/ or other relative paths, not under /tmp. Install packages with separate exec calls when needed (python3 -m pip install --user <pkg>==<ver>). There is no shared conversation context";
pub(super) const SANDBOX_PROMPT_DELEGATED_FILE_CLAUSE: &str =
    "read_delegated_file to read that one delegated file";
pub(super) const SANDBOX_PROMPT_WEB_SEARCH_CLAUSE: &str =
    "web_search when current public-web information is necessary (prefer it over curl or other network tools from exec — those often fail in the sandbox)";
pub(super) const SANDBOX_PROMPT_FOLDER_ACCESS_CLAUSE: &str = "request_folder_access only to propose that your foreground parent decide whether to ask the user — the proposal grants no access and cannot open a picker";
pub(super) const SANDBOX_PROMPT_TASK_PLAN_CLAUSE: &str = "update_task_plan to keep an ordered checklist when the task takes several steps — send the whole list every time, keep exactly one step in_progress, and update it as steps finish rather than all at the end";
pub(super) const SANDBOX_PROMPT_CLOSING: &str = "Take as many tool steps as the task genuinely needs. If the task asks for files, finish by calling done with the filenames you wrote under output/ and a short summary. If the task asks for plain text, return the complete text directly instead of calling done.";
pub(super) const SANDBOX_CHAT_ONLY_PROMPT: &str = "You are a sandboxed background assistant. Work only on the task below. You cannot access the conversation, files, folders, the public internet, external capabilities, or other agents. Return the best final text result directly from the task and your own knowledge. Do not claim to have inspected, changed, or produced anything outside this reply.";
/// Introduces the host-derived skill catalog on a tool-capable run. Names,
/// one-line descriptions, and install pins only — never SKILL.md bodies.
pub(super) const SANDBOX_PROMPT_SKILLS_INTRO: &str = "Document skills available in this workspace (before producing a listed kind of file, read `.tidebreak/skills/<name>/SKILL.md` via exec and follow it; install pins are host-validated):";

/// Compose the run's instructions for the surface it actually has.
///
/// A chat-only route returns a final-text contract before any named capability
/// is composed. Tool-capable runs retain the normal isolated executor prompt.
///
/// A vendor-searching run still has `web_search` — the model provider runs it,
/// but the model names and uses it the same way — so only a run with no search
/// at all loses the clause, exactly as a foreground turn's prompt does.
///
/// `skills` / `plugins` are the same host-derived catalogs a foreground turn
/// composes from (`ConfiguredExecProvider::skill_catalog` /
/// `plugin_catalog`). An empty catalog omits the skills section rather than
/// inventing entries or claiming none exist when the host has no surface.
pub(super) fn sandbox_system_prompt(
    delegated_file_available: bool,
    web_search: TurnWebSearch,
    tools_supported: bool,
    skills: &[SkillPackage],
    plugins: &[PluginPackage],
) -> String {
    if !tools_supported {
        return SANDBOX_CHAT_ONLY_PROMPT.to_owned();
    }
    let mut clauses = Vec::with_capacity(5);
    // Exec is always on a tool-capable route; name its argv contract first so
    // the model does not burn early steps on malformed command lines.
    clauses.push(SANDBOX_PROMPT_EXEC_CLAUSE);
    if delegated_file_available {
        clauses.push(SANDBOX_PROMPT_DELEGATED_FILE_CLAUSE);
    }
    if web_search != TurnWebSearch::Off {
        clauses.push(SANDBOX_PROMPT_WEB_SEARCH_CLAUSE);
    }
    clauses.push(SANDBOX_PROMPT_TASK_PLAN_CLAUSE);
    clauses.push(SANDBOX_PROMPT_FOLDER_ACCESS_CLAUSE);
    let last = clauses
        .pop()
        .expect("folder-access clause is always present");
    let mut tools = String::from("Use ");
    for clause in &clauses {
        tools.push_str(clause);
        tools.push_str(", ");
    }
    if !clauses.is_empty() {
        tools.push_str("and ");
    }
    tools.push_str(last);
    tools.push('.');
    let preamble = if delegated_file_available {
        SANDBOX_DELEGATED_FILE_PROMPT_PREAMBLE
    } else {
        SANDBOX_PROMPT_PREAMBLE
    };
    let mut prompt = format!("{preamble} {tools} {SANDBOX_PROMPT_CLOSING}");
    if let Some(summary) = sandbox_skills_summary(skills, plugins) {
        prompt.push(' ');
        prompt.push_str(&summary);
    }
    prompt
}

/// Concise skills block for a tool-capable sandbox run: catalog lines with
/// install pins, grouped the same way the foreground catalog groups them.
///
/// Returns `None` when every entry failed validation or the host passed an
/// empty catalog, so a headless embedding with no skills stays silent.
pub(super) fn sandbox_skills_summary(
    skills: &[SkillPackage],
    plugins: &[PluginPackage],
) -> Option<String> {
    let lines = skill_summary_catalog_lines(skills, plugins);
    if lines.is_empty() {
        return None;
    }
    // Bound how many skill lines a single prompt carries. The host catalog is
    // already small; this is a hard ceiling against a future oversized load.
    const MAX_SKILL_LINES: usize = 24;
    let mut body = String::from(SANDBOX_PROMPT_SKILLS_INTRO);
    for line in lines.into_iter().take(MAX_SKILL_LINES) {
        body.push(' ');
        body.push_str(&line);
    }
    Some(body)
}

/// Progress lines one step may publish for searches the provider ran itself.
///
/// The vendor budget already bounds these; this only keeps a misreporting
/// adapter from filling the run's observation feed from one step.
pub(super) const MAX_PROVIDER_EXECUTED_RECORDS: usize = 16;

#[derive(Debug, Clone, Copy)]
pub(crate) struct SandboxAgentRunWorkerConfig {
    pub(crate) lease: Duration,
    pub(crate) heartbeat: Duration,
    pub(crate) idle_min: Duration,
    pub(crate) idle_cap: Duration,
    pub(crate) failure_delay: Duration,
    /// Ceiling on the lane's own backoff after consecutive iteration errors,
    /// so a store outage is not polled at a fixed rate forever.
    pub(crate) failure_delay_cap: Duration,
    pub(crate) retry: RetrySchedule,
    pub(crate) max_concurrency: usize,
    pub(crate) max_running_global: u32,
    pub(crate) max_running_per_chat: u32,
    pub(crate) delegated_file_executor_enabled: bool,
    #[cfg(test)]
    pub(crate) suppress_resolver_heartbeats: bool,
}

impl Default for SandboxAgentRunWorkerConfig {
    fn default() -> Self {
        Self {
            lease: Duration::from_secs(60),
            heartbeat: Duration::from_secs(15),
            idle_min: Duration::from_millis(250),
            idle_cap: Duration::from_secs(5),
            failure_delay: Duration::from_secs(1),
            failure_delay_cap: Duration::from_secs(30),
            // A parent turn may be waiting on this run, but nothing it retries
            // recovers in milliseconds: a sandbox that is still provisioning,
            // a provider that just refused, a delegated resource that has not
            // appeared yet. Retrying inside a second only spends an attempt
            // on the same unfinished state, so the first wait is five
            // seconds. The envelope matches the run's own wall-clock deadline,
            // which the database already enforces.
            retry: RetrySchedule::new(
                Duration::from_secs(5),
                Duration::from_secs(60),
                Duration::from_secs(60 * 60),
            ),
            max_concurrency: 4,
            max_running_global: 4,
            max_running_per_chat: 2,
            delegated_file_executor_enabled: false,
            #[cfg(test)]
            suppress_resolver_heartbeats: false,
        }
    }
}

impl SandboxAgentRunWorkerConfig {
    #[must_use]
    pub(crate) const fn with_delegated_file_executor(mut self, enabled: bool) -> Self {
        self.delegated_file_executor_enabled = enabled;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SandboxAgentRunWorkerOutcome {
    Idle,
    Completed(tidebreak_core::AgentRunId),
    RetryScheduled(tidebreak_core::AgentRunId),
    Failed(tidebreak_core::AgentRunId),
    Cancelled(tidebreak_core::AgentRunId),
    CheckedIn(tidebreak_core::AgentRunId),
    ParentWaitSetResumed(tidebreak_core::CallId),
    ToolCheckpointed(tidebreak_core::CallId),
    LeaseLost(tidebreak_core::AgentRunId),
}

impl LaneOutcome for SandboxAgentRunWorkerOutcome {
    fn lane_step(&self) -> LaneStep {
        match self {
            Self::Idle => LaneStep::Idle,
            _ => LaneStep::Worked,
        }
    }
}

#[derive(Clone)]
pub(crate) struct SandboxAgentRunWorker {
    pub(crate) store: Arc<dyn Store>,
    pub(crate) secrets: Arc<dyn SecretProvider>,
    pub(crate) resolver: Arc<dyn ProviderResolver>,
    pub(crate) wake: Arc<Notify>,
    pub(crate) turn_wake: Arc<Notify>,
    pub(crate) events: Arc<EventBus>,
    pub(crate) attempts: Arc<SandboxAttemptGuard>,
    #[cfg(test)]
    pub(crate) fail_wait_set_resume_responses: Arc<AtomicUsize>,
    /// Deterministic fault seam for cancellation-finalization accounting.
    /// Production builds do not carry it; worker tests use it to hold the
    /// exact post-quiescence CAS unavailable across the execution lease.
    #[cfg(test)]
    pub(crate) fail_cancellation_accounting: Arc<AtomicBool>,
    #[cfg(test)]
    pub(crate) cancellation_accounting_failure_observed: Arc<Notify>,
    #[cfg(test)]
    pub(crate) cancellation_accounting_calls: Arc<AtomicUsize>,
    pub(crate) agent_config: AgentConfig,
    /// Each run receives a directory under this private root. The initial
    /// no-tools executor does not open it; retaining the boundary now means a
    /// future sandbox-safe tool adapter must be given an exact per-run handle
    /// rather than a chat or project path.
    pub(crate) private_scratch_root: Option<PathBuf>,
    /// Same host code-execution surface the foreground turn uses for its skill
    /// catalog. Absent on a headless embedding with no exec provider — the
    /// sandbox prompt then carries no skills section.
    pub(crate) code_execution: Option<Arc<ConfiguredExecProvider>>,
    pub(crate) config: SandboxAgentRunWorkerConfig,
}
