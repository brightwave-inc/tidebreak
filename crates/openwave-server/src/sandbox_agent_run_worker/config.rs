//! Sandbox agent-run worker configuration and system prompt.

use std::path::PathBuf;
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use std::time::Duration;

use openwave_core::{AgentConfig, SecretProvider, Store, TurnWebSearch};
use tokio::sync::Notify;

use crate::bus::EventBus;
use crate::resolver::ProviderResolver;
use crate::retry::RetrySchedule;
use crate::state::SandboxAttemptGuard;

/// Fixed instruction set for the initial isolated executor, up to the sentence
/// that names the run's tools.
///
/// Deliberately do not inherit the foreground system prompt: it may describe
/// interactive tools or conversation-wide responsibilities that are outside a
/// depth-one child run's authority.
pub(super) const SANDBOX_PROMPT_PREAMBLE: &str = "You are a sandboxed background agent. Work only on the delegated task below. You cannot access the conversation, the user's files, connected folders, or other agents. You do have your own private workspace: use exec to run commands in it, and write every deliverable the task asks for as a file under output/ — each file you write there is published to the user as a durable output named by its own filename, so name those files the way you want the user to see them. Prefer producing a file over describing what a file would contain.";
pub(super) const SANDBOX_DELEGATED_FILE_PROMPT_PREAMBLE: &str = "You are a sandboxed background agent. Work only on the delegated task below. You cannot access the conversation, the user's files, connected folders, or other agents, except for the one exact file explicitly delegated to this run. You do have your own private workspace: use exec to run commands in it, and write every deliverable the task asks for as a file under output/ — each file you write there is published to the user as a durable output named by its own filename, so name those files the way you want the user to see them. Prefer producing a file over describing what a file would contain.";
pub(super) const SANDBOX_PROMPT_DELEGATED_FILE_CLAUSE: &str =
    "read_delegated_file to read that one delegated file";
pub(super) const SANDBOX_PROMPT_WEB_SEARCH_CLAUSE: &str =
    "web_search when current public-web information is necessary";
pub(super) const SANDBOX_PROMPT_FOLDER_ACCESS_CLAUSE: &str = "request_folder_access only to propose that your foreground parent decide whether to ask the user — the proposal grants no access and cannot open a picker";
pub(super) const SANDBOX_PROMPT_TASK_PLAN_CLAUSE: &str = "update_task_plan to keep an ordered checklist when the task takes several steps — send the whole list every time, keep exactly one step in_progress, and update it as steps finish rather than all at the end";
pub(super) const SANDBOX_PROMPT_CLOSING: &str = "Take as many tool steps as the task genuinely needs, then finish by calling done with the filenames you wrote under output/ and a short summary of what you produced.";

/// Compose the run's instructions for the surface it actually has.
///
/// A vendor-searching run still has `web_search` — the model provider runs it,
/// but the model names and uses it the same way — so only a run with no search
/// at all loses the clause, exactly as a foreground turn's prompt does.
pub(super) fn sandbox_system_prompt(
    delegated_file_available: bool,
    web_search: TurnWebSearch,
) -> String {
    let mut clauses = Vec::with_capacity(4);
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
    format!("{preamble} {tools} {SANDBOX_PROMPT_CLOSING}")
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
    Completed(openwave_core::AgentRunId),
    RetryScheduled(openwave_core::AgentRunId),
    Failed(openwave_core::AgentRunId),
    Cancelled(openwave_core::AgentRunId),
    ParentWaitSetResumed(openwave_core::CallId),
    ToolCheckpointed(openwave_core::CallId),
    LeaseLost(openwave_core::AgentRunId),
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
    pub(crate) agent_config: AgentConfig,
    /// Each run receives a directory under this private root. The initial
    /// no-tools executor does not open it; retaining the boundary now means a
    /// future sandbox-safe tool adapter must be given an exact per-run handle
    /// rather than a chat or project path.
    pub(crate) private_scratch_root: Option<PathBuf>,
    pub(crate) config: SandboxAgentRunWorkerConfig,
}
