//! Route handlers extracted from the parent `routes` module.

use axum::extract::State;
use axum::http::StatusCode;
use chrono::Utc;
use serde::{Deserialize, Serialize};

use tidebreak_code_execution::ExecProviderKind;
use tidebreak_core::{
    AgentRun, AgentRunExecutionLocation, AgentRunStatus, AgentRunTier, CallId, ChatId,
    RequestAgentRunCancellationOutcome, SandboxToolCall, SandboxToolCallStatus, TurnId,
};

use crate::agent_control_tools::SandboxCancellationAcceleration;
use crate::code_execution;
use crate::error::ServerError;
use crate::extract::{Json, Path, Query};
use crate::scoped_store::ScopedStore;
use crate::state::{AppState, SandboxSteerRefusal};

/// Host-selected backend that runs `exec` tool calls.
///
/// Distinct from [`AgentRunExecutionLocation`], which names where the agent
/// *run loop* itself executes (`in_process` vs `container`). A background run
/// can be in-process while its shell work still lands on e2b, docker, or
/// daytona — this field is that backend, or `off` when code execution is
/// disabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum ExecProviderSnapshot {
    Local,
    E2b,
    Daytona,
    Docker,
    Off,
}

impl ExecProviderSnapshot {
    fn from_config(provider: Option<ExecProviderKind>) -> Self {
        match provider {
            Some(ExecProviderKind::Local) => Self::Local,
            Some(ExecProviderKind::E2b) => Self::E2b,
            Some(ExecProviderKind::Daytona) => Self::Daytona,
            Some(ExecProviderKind::Docker) => Self::Docker,
            // `ExecProviderKind` is non-exhaustive; an unknown future
            // variant is not something a renderer can name yet.
            Some(_) | None => Self::Off,
        }
    }
}

/// Renderer-safe state for one agent run.
///
/// Worker lease tokens, scheduling budgets, and other executor-facing fields
/// intentionally remain inside the server/store boundary.
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct AgentRunSnapshot {
    pub id: tidebreak_core::AgentRunId,
    pub parent_id: Option<tidebreak_core::AgentRunId>,
    pub tier: AgentRunTier,
    pub execution_location: AgentRunExecutionLocation,
    /// Active host code-execution backend for `exec`, not the run-loop seat.
    ///
    /// See [`ExecProviderSnapshot`]. Read from the current host
    /// setting at list time — the same selection the next `exec` would use.
    pub code_execution_provider: ExecProviderSnapshot,
    pub status: AgentRunStatus,
    /// Completed provider calls accumulated across every attempt.
    pub model_steps: i32,
    /// Disjoint provider usage accumulated across every attempt. Providers
    /// without cache telemetry leave both cache fields at zero.
    pub usage: AgentRunUsageSnapshot,
    /// The exact bounded task delegated by the visible spawn step.
    pub task: Option<String>,
    pub started_at: Option<chrono::DateTime<Utc>>,
    pub finished_at: Option<chrono::DateTime<Utc>>,
    /// Stable, bounded classification suitable for renderer display.
    pub last_error_code: Option<String>,
    /// The currently checkpointed, renderer-safe activity, if any.
    ///
    /// This is intentionally a small fixed vocabulary. It never exposes tool
    /// arguments, results, provider call identities, executor leases, or raw
    /// executor diagnostics.
    pub activity: Option<AgentActivitySnapshot>,
    /// Files a background run submitted as its deliverables, in its own order.
    ///
    /// A background run produces outputs by writing files and submitting them
    /// by name; nothing here is host-authored, and a run that submitted nothing
    /// carries an empty list.
    pub submitted_outputs: Vec<SubmittedOutputSnapshot>,
    /// How far this run's own task plan has got, when it keeps one.
    ///
    /// The full list is its own route; the snapshot carries only what a status
    /// row needs — how many steps are done, and the one step being worked on.
    ///
    /// Omitted rather than null when there is no plan, which keeps the wire
    /// additive: every run before this field existed, and every foreground
    /// coordinator, reads back in the shape it always had.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub task_plan: Option<AgentRunTaskPlanProgress>,
    /// Bounded terminal display text returned to the parent, if settled.
    pub terminal_text: Option<String>,
    pub created_at: chrono::DateTime<Utc>,
    pub updated_at: chrono::DateTime<Utc>,
    // This is an Tidebreak call id, not a provider call identity. It lets a
    // transcript observer attach this durable status to its exact spawning
    // step without exposing delegated input or executor data.
    pub spawn_call_id: Option<tidebreak_core::CallId>,
}

impl AgentRunSnapshot {
    fn from_run(
        run: AgentRun,
        activity: Option<AgentActivitySnapshot>,
        terminal_text: Option<String>,
        submitted_outputs: Vec<SubmittedOutputSnapshot>,
        task_plan: Option<AgentRunTaskPlanProgress>,
        code_execution_provider: ExecProviderSnapshot,
    ) -> Self {
        Self {
            id: run.id,
            parent_id: run.parent_id,
            spawn_call_id: run.spawn_call_id,
            tier: run.tier,
            execution_location: run.execution_location,
            code_execution_provider,
            status: run.status,
            model_steps: run.model_steps,
            usage: run.usage.into(),
            task: run.input,
            started_at: run.started_at,
            finished_at: run.finished_at,
            last_error_code: run.last_error_code,
            activity,
            submitted_outputs,
            task_plan,
            terminal_text,
            created_at: run.created_at,
            updated_at: run.updated_at,
        }
    }
}

/// Renderer-safe disjoint token accounting for one background run.
#[derive(Debug, Clone, Copy, Default, Serialize, ts_rs::TS)]
pub struct AgentRunUsageSnapshot {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_input_tokens: u32,
    pub cache_creation_input_tokens: u32,
}

impl From<tidebreak_core::Usage> for AgentRunUsageSnapshot {
    fn from(usage: tidebreak_core::Usage) -> Self {
        Self {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_read_input_tokens: usage.cache_read_input_tokens,
            cache_creation_input_tokens: usage.cache_creation_input_tokens,
        }
    }
}

/// One file a background run submitted, as the renderer sees it.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
pub struct SubmittedOutputSnapshot {
    pub output_id: tidebreak_core::OutputId,
    /// The name the run gave the file, which is the output's name.
    pub filename: String,
}

/// A run's plan as a status row needs it: the count, and the current step.
///
/// Step text is model-authored, like the command and query headlines the
/// activity history carries. The difference is that those go through the
/// renderer clamp on the way out and this does not, so the tool boundary
/// rejects on the way in against the very same predicate: a step longer than
/// [`tidebreak_core::MAX_TASK_PLAN_STEP_CHARS`], or carrying any character the
/// preview clamp would strip — control characters, the line and paragraph
/// separators, the bidi overrides and isolates — never becomes a stored step.
/// Copying it as stored is therefore not a gap in the clamp; it is the same
/// rule enforced one surface earlier.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
pub struct AgentRunTaskPlanProgress {
    pub completed: u32,
    pub total: u32,
    /// The one step marked `in_progress`, when there is one.
    pub current: Option<String>,
    pub updated_at: chrono::DateTime<Utc>,
}

impl AgentRunTaskPlanProgress {
    fn from_plan(plan: &tidebreak_core::AgentRunTaskPlan) -> Self {
        Self {
            completed: u32::try_from(
                plan.steps
                    .iter()
                    .filter(|step| step.status == tidebreak_core::TaskPlanStepStatus::Completed)
                    .count(),
            )
            .unwrap_or(u32::MAX),
            total: u32::try_from(plan.steps.len()).unwrap_or(u32::MAX),
            current: plan
                .steps
                .iter()
                .find(|step| step.status == tidebreak_core::TaskPlanStepStatus::InProgress)
                .map(|step| step.content.clone()),
            updated_at: plan.updated_at,
        }
    }
}

/// Fixed, renderer-safe names for supported live work.
///
/// Adding a durable tool does not automatically expose it to a renderer: it
/// must be deliberately admitted here with a safe label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum AgentActivityKind {
    Exec,
    WebSearch,
    UpdateTaskPlan,
    ReadDelegatedFile,
    ListConnectedFolders,
    ListFolder,
    ReadConnectedFile,
    ImportConnectedFile,
}

impl AgentActivityKind {
    /// The wire spelling, for a client that lists activity as text. Pinned to
    /// the serde rendering by `activity_names_match_the_wire`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Exec => "exec",
            Self::WebSearch => "web_search",
            Self::UpdateTaskPlan => "update_task_plan",
            Self::ReadDelegatedFile => "read_delegated_file",
            Self::ListConnectedFolders => "list_connected_folders",
            Self::ListFolder => "list_folder",
            Self::ReadConnectedFile => "read_connected_file",
            Self::ImportConnectedFile => "import_connected_file",
        }
    }
}

/// Coarse checkpoint lifecycle suitable for display.
///
/// This intentionally does not mirror all durable executor states; only live
/// work is represented, and terminal checkpoints produce no activity.
#[derive(Debug, Clone, Copy, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum AgentActivityStatus {
    Waiting,
    Running,
}

/// Renderer-safe projection of one live supported checkpoint.
#[derive(Debug, Clone, Copy, Serialize, ts_rs::TS)]
pub struct AgentActivitySnapshot {
    pub kind: AgentActivityKind,
    pub status: AgentActivityStatus,
}

fn sandbox_activity(calls: &[SandboxToolCall]) -> Option<AgentActivitySnapshot> {
    // A sandbox has at most one parked step outstanding, but that step can hold
    // several calls at once. The one being executed is what the run is actually
    // doing, so it wins; otherwise the first still-live call in emission order
    // stands for the step. Terminal checkpoints produce nothing, so an older
    // completed activity cannot linger after the run has advanced.
    let call = calls
        .iter()
        .find(|call| call.status == SandboxToolCallStatus::Claimed)
        .or_else(|| calls.iter().find(|call| !call.status.is_terminal()))?;
    let kind = match call.name.as_str() {
        tidebreak_core::SANDBOX_EXEC_TOOL => AgentActivityKind::Exec,
        "web_search" => AgentActivityKind::WebSearch,
        tidebreak_core::UPDATE_TASK_PLAN_TOOL => AgentActivityKind::UpdateTaskPlan,
        tidebreak_core::SANDBOX_READ_DELEGATED_FILE_TOOL => AgentActivityKind::ReadDelegatedFile,
        // Unknown tool names are executor data, not a renderer API contract.
        _ => return None,
    };
    let status = match call.status {
        SandboxToolCallStatus::Accepted | SandboxToolCallStatus::RetryWait => {
            AgentActivityStatus::Waiting
        }
        SandboxToolCallStatus::Claimed => AgentActivityStatus::Running,
        SandboxToolCallStatus::Completed
        | SandboxToolCallStatus::Failed
        | SandboxToolCallStatus::Cancelled => return None,
        _ => return None,
    };
    Some(AgentActivitySnapshot { kind, status })
}

/// Coarse, renderer-safe lifecycle for one historical activity entry.
///
/// Unlike [`AgentActivityStatus`], which only names live work, this also
/// admits the three terminal outcomes so a settled step can be shown in an
/// ordered timeline. It carries no failure detail: a failed step is only
/// "failed", never why.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum AgentActivityOutcome {
    Waiting,
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl AgentActivityOutcome {
    /// The wire spelling, for a client that lists activity as text. Pinned to
    /// the serde rendering by `activity_names_match_the_wire`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Waiting => "waiting",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// One renderer-safe entry in a background run's ordered activity history.
///
/// Built on read from durable sandbox tool calls and their immutable receipts.
/// `detail` admits bounded model-authored command/argument/query text, which may
/// repeat anything the child already saw and is not covered by the host-field
/// non-disclosure guarantee. Stored result text is copied in one place only: a
/// settled `exec` step carries its receipt's bounded tail, because that text is
/// the command's own output from a private workspace and is what makes a failed
/// step readable. Web-search and delegated-file results stay server-side. The
/// other host-derived values are the numeric exit code parsed from a receipt's
/// first line and the delegated file's leaf name. Full broker paths and root
/// identities, provider identities, executor leases, and diagnostics are never
/// copied.
///
/// No separate activity-history shape is persisted. The optional field keeps
/// the wire additive for older clients and lets calls without derivable detail
/// retain the original `{kind, outcome, at}` shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
pub struct AgentActivityHistoryItem {
    pub kind: AgentActivityKind,
    pub outcome: AgentActivityOutcome,
    pub at: chrono::DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub detail: Option<tidebreak_core::AgentActivityDetail>,
}

/// Project a background run's durable sandbox tool calls into an ordered,
/// renderer-safe activity history.
///
/// The store returns calls in durable creation order, so the projection keeps
/// that order. Tool names outside the admitted vocabulary are executor data,
/// not a renderer contract, and are skipped rather than leaked as raw labels.
/// `delegated_file` is the run's one admission-delegated file identity, when it
/// had one; only its base name may reach a `read_delegated_file` entry. The
/// receipt map contains only terminal exec receipts, used to recover the typed
/// exit code from their first line and a bounded tail of what the command
/// printed.
fn sandbox_activity_history(
    calls: &[SandboxToolCall],
    delegated_file: Option<&str>,
    receipts: &std::collections::HashMap<CallId, tidebreak_core::SandboxToolCallReceipt>,
) -> Vec<AgentActivityHistoryItem> {
    calls
        .iter()
        .filter_map(|call| sandbox_activity_history_item(call, delegated_file, receipts))
        .collect()
}

fn sandbox_activity_history_item(
    call: &SandboxToolCall,
    delegated_file: Option<&str>,
    receipts: &std::collections::HashMap<CallId, tidebreak_core::SandboxToolCallReceipt>,
) -> Option<AgentActivityHistoryItem> {
    let kind = match call.name.as_str() {
        tidebreak_core::SANDBOX_EXEC_TOOL => AgentActivityKind::Exec,
        "web_search" => AgentActivityKind::WebSearch,
        tidebreak_core::UPDATE_TASK_PLAN_TOOL => AgentActivityKind::UpdateTaskPlan,
        tidebreak_core::SANDBOX_READ_DELEGATED_FILE_TOOL => AgentActivityKind::ReadDelegatedFile,
        // Unknown tool names are executor data, not a renderer API contract.
        _ => return None,
    };
    // A terminal step is dated by when it resolved; a live step by when it was
    // admitted. `resolved_at` is always present once terminal, but fall back to
    // the creation time rather than dropping a settled step from the timeline.
    let (outcome, at) = match call.status {
        SandboxToolCallStatus::Accepted | SandboxToolCallStatus::RetryWait => {
            (AgentActivityOutcome::Waiting, call.created_at)
        }
        SandboxToolCallStatus::Claimed => (AgentActivityOutcome::Running, call.created_at),
        SandboxToolCallStatus::Completed => (
            AgentActivityOutcome::Completed,
            call.resolved_at.unwrap_or(call.created_at),
        ),
        SandboxToolCallStatus::Failed => (
            AgentActivityOutcome::Failed,
            call.resolved_at.unwrap_or(call.created_at),
        ),
        SandboxToolCallStatus::Cancelled => (
            AgentActivityOutcome::Cancelled,
            call.resolved_at.unwrap_or(call.created_at),
        ),
        // `SandboxToolCallStatus` is non-exhaustive; an unrecognized future
        // state is executor data, not a renderer contract.
        _ => return None,
    };
    // The delegated read is argument-free, so its headline comes from the
    // admission rather than from model-authored arguments. Every other detail
    // is a bounded projection of the durable call and optional receipt.
    let detail =
        match call.name.as_str() {
            tidebreak_core::SANDBOX_READ_DELEGATED_FILE_TOOL => {
                delegated_file.and_then(tidebreak_core::AgentActivityDetail::delegated_file)
            }
            _ => tidebreak_core::AgentActivityDetail::build(&call.name, &call.arguments).map(
                |detail| match receipts.get(&call.id) {
                    Some(receipt) => detail.with_exec_result(&receipt.result),
                    None => detail,
                },
            ),
        };
    Some(AgentActivityHistoryItem {
        kind,
        outcome,
        at,
        detail,
    })
}

/// `GET /chats/{id}/agent-runs` — list renderer-safe execution state.
pub async fn list_agent_runs(
    State(state): State<AppState>,
    store: ScopedStore,
    Path(id): Path<ChatId>,
) -> Result<Json<Vec<AgentRunSnapshot>>, ServerError> {
    store.require_chat(id).await?;
    let runs = store.list_agent_runs(id).await?;
    // Host code-exec selection is independent of where the run loop sits.
    // One read covers every snapshot in the response: the next `exec` uses
    // the same setting, whether the child is foreground or background.
    let code_execution_provider = ExecProviderSnapshot::from_config(
        code_execution::read_config(&*state.store).await?.provider,
    );
    let mut snapshots = Vec::with_capacity(runs.len());
    for run in runs {
        let mut submitted_outputs = Vec::new();
        let terminal_text = match run.status {
            AgentRunStatus::Completed | AgentRunStatus::Cancelled => {
                match store.get_agent_run_result(run.id).await? {
                    Some(result) => match &result.payload {
                        // A submission's names belong to the structured field,
                        // where the reader can open each one. Repeating them in
                        // the prose block would say the same thing twice, so
                        // the text here is the run's summary alone.
                        tidebreak_core::AgentRunResultPayload::Submission { outputs, summary } => {
                            submitted_outputs.extend(outputs.iter().map(|output| {
                                SubmittedOutputSnapshot {
                                    output_id: output.output_id,
                                    filename: output.filename.clone(),
                                }
                            }));
                            Some(summary.clone())
                        }
                        _ => Some(result.text),
                    },
                    None => None,
                }
            }
            AgentRunStatus::Failed => run
                .last_error_code
                .as_deref()
                .map(|code| format!("Sandbox task failed ({code})")),
            _ => None,
        };
        let calls = store.list_sandbox_tool_calls_for_agent_run(run.id).await?;
        let task_plan = store
            .get_agent_run_task_plan(run.id)
            .await?
            .as_ref()
            .map(AgentRunTaskPlanProgress::from_plan);
        let activity = sandbox_activity(&calls);
        snapshots.push(AgentRunSnapshot::from_run(
            run,
            activity,
            terminal_text,
            submitted_outputs,
            task_plan,
            code_execution_provider,
        ));
    }
    Ok(Json(snapshots))
}

/// `GET /chats/{id}/agent-runs/{run_id}/activity` — ordered, renderer-safe
/// activity history for one background run.
///
/// This is the durable companion to the live `activity` field on a run
/// snapshot: where that field names only the single current checkpoint, this
/// returns every admitted step in order, each with a coarse terminal outcome
/// and timestamp. Each entry may add a bounded typed headline — the command,
/// exit status, and output tail a settled exec recorded, the query a web search
/// asked, or the base name of the run's one delegated file. Command, argument,
/// and query text is model-authored and may repeat information the child
/// already saw. The exec output tail is the one stored result the projection
/// copies: it is the command's own text from a private workspace. Web-search
/// and delegated-file results and host-only fields are not copied, apart from
/// the typed exit code and admitted leaf name. A missing, wrong-chat, or
/// foreground run returns `404` rather than revealing whether an unrelated run
/// identifier exists.
pub async fn list_agent_run_activity(
    store: ScopedStore,
    Path((chat_id, run_id)): Path<(ChatId, tidebreak_core::AgentRunId)>,
) -> Result<Json<Vec<AgentActivityHistoryItem>>, ServerError> {
    store.require_chat(chat_id).await?;
    let run = store
        .get_agent_run(run_id)
        .await?
        .filter(|run| run.chat_id == chat_id && run.tier == AgentRunTier::Background);
    let Some(run) = run else {
        return Err(ServerError::not_found(format!(
            "agent run {run_id} not found"
        )));
    };
    let calls = store.list_sandbox_tool_calls_for_agent_run(run.id).await?;
    // The run's one delegated file identity is needed only for its argument-free
    // read call. A missing admission leaves that entry in the original
    // detail-free shape rather than dropping the history.
    let delegated_file = if calls
        .iter()
        .any(|call| call.name == tidebreak_core::SANDBOX_READ_DELEGATED_FILE_TOOL)
    {
        store
            .get_sandbox_agent_admission(run.id)
            .await?
            .and_then(|admission| admission.resource)
            .map(|resource| resource.relative_path)
    } else {
        None
    };
    // Exit status and the printed tail live on the immutable receipts, not the
    // call rows. A missing receipt — a live step, or a call settled before
    // receipts were kept — leaves the detail without them rather than failing.
    let mut receipts = std::collections::HashMap::new();
    for call in &calls {
        if call.name == tidebreak_core::SANDBOX_EXEC_TOOL && call.status.is_terminal() {
            if let Some(receipt) = store.get_sandbox_tool_call_receipt(call.id).await? {
                receipts.insert(call.id, receipt);
            }
        }
    }
    Ok(Json(sandbox_activity_history(
        &calls,
        delegated_file.as_deref(),
        &receipts,
    )))
}

/// `GET /chats/{chat_id}/agent-runs/{run_id}/task-plan` — the full ordered
/// checklist one background run keeps, or `null`.
///
/// The snapshot carries the count and the current step because that is all a
/// status row needs; a reader opening the run wants the whole list. The steps
/// are the run's own model-authored text, bounded and single-line before
/// storage accepts them. Nothing else about the run crosses here — no
/// checkpoint arguments, receipts, leases, or diagnostics.
///
/// Read-only and bound to the exact chat: a missing, wrong-chat, or foreground
/// run returns `404`, exactly as the activity history does. A run that never
/// made a plan is not an error; it answers `null`.
pub async fn get_agent_run_task_plan(
    store: ScopedStore,
    Path((chat_id, run_id)): Path<(ChatId, tidebreak_core::AgentRunId)>,
) -> Result<Json<Option<tidebreak_core::AgentRunTaskPlan>>, ServerError> {
    store.require_chat(chat_id).await?;
    let run = store
        .get_agent_run(run_id)
        .await?
        .filter(|run| run.chat_id == chat_id && run.tier == AgentRunTier::Background);
    if run.is_none() {
        return Err(ServerError::not_found(format!(
            "agent run {run_id} not found"
        )));
    }
    Ok(Json(store.get_agent_run_task_plan(run_id).await?))
}

/// One line of live progress a background run published, as the renderer sees
/// it.
///
/// The text is the run's own bounded narration — the same class of prose the
/// terminal `terminal_text` already carries, published while the run is still
/// working instead of only at the end. It is model-authored and may repeat
/// information the run already saw. Stored tool records and host-owned fields
/// are not copied directly into it. Typed activity headlines are projected
/// separately.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
pub struct AgentRunProgressLine {
    /// Monotonic per-run ordering. Pass the page's `next_sequence` back as
    /// `after_sequence` to read only what has arrived since.
    pub sequence: i64,
    pub text: String,
    pub at: chrono::DateTime<Utc>,
}

/// One resumable page of a background run's live progress.
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct AgentRunProgressPage {
    pub entries: Vec<AgentRunProgressLine>,
    /// The cursor to resume from: the highest sequence in this page, or the
    /// requested cursor when the page is empty. A reader that polls with this
    /// value never re-reads a line it already has.
    pub next_sequence: i64,
}

/// Query for `GET /chats/{chat_id}/agent-runs/{run_id}/progress`.
#[derive(Debug, Deserialize)]
pub struct AgentRunProgressQuery {
    /// Return only lines strictly newer than this sequence; `0` (the default)
    /// starts from the oldest line retention still holds.
    #[serde(default)]
    pub after_sequence: i64,
    /// Maximum lines to return, clamped to the read model's own bound.
    pub limit: Option<u64>,
}

/// `GET /chats/{chat_id}/agent-runs/{run_id}/progress` — the resumable live
/// progress stream for one background run.
///
/// The run snapshot says what state a child is in and the activity projections
/// say which step it is on; neither says what the child is actually doing. This
/// is that: the ordered lines the run itself published, readable while it is
/// still running rather than only once it submits a result. Because each line
/// carries a monotonic sequence, an observer polls with the cursor it last saw
/// and receives only what is new.
///
/// Read-only, and bound to the exact chat: a missing, wrong-chat, or foreground
/// run returns `404` rather than revealing whether an unrelated run identifier
/// exists.
pub async fn list_agent_run_progress(
    store: ScopedStore,
    Path((chat_id, run_id)): Path<(ChatId, tidebreak_core::AgentRunId)>,
    Query(query): Query<AgentRunProgressQuery>,
) -> Result<Json<AgentRunProgressPage>, ServerError> {
    store.require_chat(chat_id).await?;
    let run = store
        .get_agent_run(run_id)
        .await?
        .filter(|run| run.chat_id == chat_id && run.tier == AgentRunTier::Background);
    if run.is_none() {
        return Err(ServerError::not_found(format!(
            "agent run {run_id} not found"
        )));
    }
    let after_sequence = query.after_sequence.max(0);
    let limit = query
        .limit
        .unwrap_or(tidebreak_core::AgentRunProgressEntry::DEFAULT_PAGE);
    let entries = store
        .list_agent_run_progress(run_id, after_sequence, limit)
        .await?;
    let next_sequence = entries
        .last()
        .map_or(after_sequence, |entry| entry.sequence);
    Ok(Json(AgentRunProgressPage {
        entries: entries
            .into_iter()
            .map(|entry| AgentRunProgressLine {
                sequence: entry.sequence,
                text: entry.text,
                at: entry.created_at,
            })
            .collect(),
        next_sequence,
    }))
}

/// Closed renderer-safe acknowledgement for sandbox cancellation.
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct AgentRunCancellationSnapshot {
    pub id: tidebreak_core::AgentRunId,
    pub status: AgentRunCancellationStatus,
}

#[derive(Debug, Clone, Copy, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum AgentRunCancellationStatus {
    Cancelling,
    Cancelled,
}

/// `POST /chats/{chat_id}/agent-runs/{run_id}/cancel` — durably request
/// cancellation of one sandbox child.
///
/// The durable transition commits before any process-local signal is sent.
/// Exact retries of cancelling or cancelled work remain accepted. Foreground,
/// wrong-chat, successful, and failed runs are rejected without exposing
/// executor details.
pub async fn post_agent_run_cancel(
    State(state): State<AppState>,
    store: ScopedStore,
    Path((chat_id, run_id)): Path<(ChatId, tidebreak_core::AgentRunId)>,
) -> Result<(StatusCode, Json<AgentRunCancellationSnapshot>), ServerError> {
    store.require_chat(chat_id).await?;
    let Some(run) = store.get_agent_run(run_id).await? else {
        return Err(ServerError::conflict("sandbox run is not cancellable"));
    };
    if run.chat_id != chat_id || run.tier != AgentRunTier::Background {
        return Err(ServerError::conflict("sandbox run is not cancellable"));
    }

    let mut outcome = None;
    for _ in 0..8 {
        if let Some(resolved) = store.request_agent_run_cancellation(run_id).await? {
            outcome = Some(resolved);
            break;
        }
        let Some(current) = store.get_agent_run(run_id).await? else {
            return Err(ServerError::conflict("sandbox run is not cancellable"));
        };
        if current.chat_id != chat_id || current.tier != AgentRunTier::Background {
            return Err(ServerError::conflict("sandbox run is not cancellable"));
        }
        tokio::task::yield_now().await;
    }
    let Some(outcome) = outcome else {
        return Err(ServerError::conflict(
            "sandbox run cancellation could not be serialized",
        ));
    };

    let (run, status) = match outcome {
        RequestAgentRunCancellationOutcome::Requested(run) => {
            let status = AgentRunCancellationStatus::Cancelling;
            (run, status)
        }
        RequestAgentRunCancellationOutcome::Cancelled(run) => {
            (run, AgentRunCancellationStatus::Cancelled)
        }
        RequestAgentRunCancellationOutcome::Existing(run)
            if run.status == AgentRunStatus::Cancelled =>
        {
            (run, AgentRunCancellationStatus::Cancelled)
        }
        RequestAgentRunCancellationOutcome::Existing(run)
            if run.status == AgentRunStatus::Cancelling =>
        {
            (run, AgentRunCancellationStatus::Cancelling)
        }
        RequestAgentRunCancellationOutcome::AlreadyTerminal(_)
        | RequestAgentRunCancellationOutcome::Existing(_) => {
            return Err(ServerError::conflict("sandbox run is not cancellable"));
        }
    };

    signal_sandbox_run_after_commit(&state, run.id, run.lease_token).await;
    Ok((
        StatusCode::ACCEPTED,
        Json(AgentRunCancellationSnapshot { id: run.id, status }),
    ))
}

/// Body of `POST /chats/{chat_id}/agent-runs/{run_id}/resume`.
#[derive(Debug, Default, serde::Deserialize)]
pub struct AgentRunResumeBody {
    /// Optional direction folded into the run's task before it continues.
    #[serde(default)]
    pub guidance: Option<String>,
}

/// `POST /chats/{chat_id}/agent-runs/{run_id}/resume` — resume a background
/// run that checked in, granting it another cadence window.
///
/// Guidance, when present, is appended durably to the run's task text so
/// every later claim replays it. A run that is not paused in `needs_input`
/// is refused with `409` and nothing changes.
pub async fn post_agent_run_resume(
    State(state): State<AppState>,
    store: ScopedStore,
    Path((chat_id, run_id)): Path<(ChatId, tidebreak_core::AgentRunId)>,
    Json(body): Json<AgentRunResumeBody>,
) -> Result<StatusCode, ServerError> {
    let guidance = body
        .guidance
        .as_deref()
        .map(str::trim)
        .filter(|guidance| !guidance.is_empty());
    store.require_chat(chat_id).await?;
    let run = store
        .get_agent_run(run_id)
        .await?
        .filter(|run| run.chat_id == chat_id && run.tier == AgentRunTier::Background);
    let Some(_) = run else {
        return Err(ServerError::not_found(format!(
            "agent run {run_id} not found"
        )));
    };
    match store
        .resume_agent_run_from_checkin(run_id, guidance)
        .await?
    {
        Some(_) => {
            // The scheduler should notice the freshly available run promptly.
            state.agent_run_wake.notify_one();
            Ok(StatusCode::ACCEPTED)
        }
        None => Err(ServerError::conflict(
            "sandbox run is not paused at a check-in",
        )),
    }
}

/// Body of `POST /chats/{chat_id}/agent-runs/{run_id}/steer`.
#[derive(Debug, Deserialize)]
pub struct AgentRunSteerBody {
    /// The instruction to hand the running sandbox agent.
    pub content: String,
}

/// `POST /chats/{chat_id}/agent-runs/{run_id}/steer` — hand one mid-run
/// instruction to a sandbox-resident child that is running right now.
///
/// Unlike turn steering, this is **attached-only and not durable**: the
/// instruction travels over the connection the container driver is holding, and
/// the sandbox folds it into its next model step. A run this process holds no
/// connection to is refused with `409` and nothing is queued, so a caller is
/// never told an instruction was accepted that no agent will ever read.
/// `202 Accepted` means a live connection took it. Foreground, wrong-chat, and
/// terminal runs are rejected without exposing executor details.
pub async fn post_agent_run_steer(
    State(state): State<AppState>,
    store: ScopedStore,
    Path((chat_id, run_id)): Path<(ChatId, tidebreak_core::AgentRunId)>,
    Json(body): Json<AgentRunSteerBody>,
) -> Result<StatusCode, ServerError> {
    let content = body.content.trim().to_owned();
    if content.is_empty()
        || content.contains('\0')
        || content.len() > tidebreak_sandbox_protocol::steer::MAX_STEER_BYTES
    {
        return Err(ServerError::bad_request(
            "steering content must be non-empty, contain no NUL characters, and fit the size limit",
        ));
    }
    store.require_chat(chat_id).await?;
    let run = store
        .get_agent_run(run_id)
        .await?
        .filter(|run| run.chat_id == chat_id && run.tier == AgentRunTier::Background);
    let Some(run) = run else {
        return Err(ServerError::not_found(format!(
            "agent run {run_id} not found"
        )));
    };
    if run.status != AgentRunStatus::Running {
        return Err(ServerError::conflict("sandbox run is not steerable"));
    }
    match state.sandbox_steering.steer(run_id, content) {
        Ok(()) => Ok(StatusCode::ACCEPTED),
        Err(SandboxSteerRefusal::NotAttached) => Err(ServerError::conflict(
            "sandbox run is not attached; steering is not queued",
        )),
        Err(SandboxSteerRefusal::Backlogged) => Err(ServerError::conflict(
            "sandbox run has not consumed its pending steering yet",
        )),
    }
}

/// Best-effort local acceleration after a sandbox cancellation has committed.
///
/// The immutable receipts provide exact attempt identities. Missing receipts,
/// transient read failures, and absent local workers are harmless because the
/// durable state machine remains authoritative and its workers will eventually
/// observe the cancellation through heartbeats, lease expiry, or terminal
/// write fencing.
pub(super) async fn signal_sandbox_run_after_commit(
    state: &AppState,
    run_id: tidebreak_core::AgentRunId,
    committed_lease_token: Option<uuid::Uuid>,
) {
    SandboxCancellationAcceleration::new(
        state.store.clone(),
        state.sandbox_attempts.clone(),
        state.sandbox_steering.clone(),
        state.agent_run_wake.clone(),
    )
    .signal_after_commit(run_id, committed_lease_token)
    .await;
}

/// Signal only children durably owned by the cancelled origin turn.
pub(super) async fn signal_origin_sandbox_runs_after_commit(
    state: &AppState,
    chat_id: ChatId,
    origin_turn_id: TurnId,
) {
    let Ok(runs) = state.store.list_agent_runs(chat_id).await else {
        return;
    };
    for run in runs {
        if run.tier != AgentRunTier::Background
            || !matches!(
                run.status,
                AgentRunStatus::Cancelling | AgentRunStatus::Cancelled
            )
        {
            continue;
        }
        let Ok(Some(admission)) = state.store.get_sandbox_agent_admission(run.id).await else {
            continue;
        };
        if admission.origin_turn_id == origin_turn_id {
            signal_sandbox_run_after_commit(state, run.id, None).await;
        }
    }
}
