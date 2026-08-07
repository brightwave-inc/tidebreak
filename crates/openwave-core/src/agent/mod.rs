//! The agent loop: the turn engine that drives a conversation.
//!
//! One [`Agent`] ties together a [`ModelProvider`], a [`ToolRegistry`], and a
//! [`Store`], and runs a *turn* — one user input through to a final answer —
//! emitting [`AgentEvent`]s as it goes.
//!
//! Per turn the loop: assembles the request → streams the model call →, if the
//! model called tools, runs them and feeds the results back → repeats until the
//! model stops, bounded by a max-steps guard.
//!
//! v1 scope (deliberately small; each is a tracked follow-up):
//! - read-only tool calls may run concurrently; workspace writes and calls that
//!   require a checkpoint or approval stay ordered;
//! - approval is **auto** for `ReadOnly`/`Workspace`; `Sensitive` parks via an
//!   [`ApprovalGate`] until approve/reject unless a standing grant covers it;
//! - context reduction is deterministic floor+restore, with optional semantic
//!   checkpoints on the utility model when compaction policy triggers;
//!   retries with progressive reduction on provider prompt-too-long errors.

mod context;
mod dispatch;
mod events;
mod registry;
mod transcript;
mod turn;
mod types;

#[cfg(all(test, feature = "sqlite", feature = "tools"))]
#[path = "tests/mod.rs"]
mod tests;

#[cfg(all(test, feature = "sqlite"))]
#[path = "tests/image_hydration.rs"]
mod image_hydration_tests;

pub use events::ClaimedAgentEvent;
pub use registry::ToolRegistry;
#[cfg(test)]
pub(crate) use transcript::rebuild_transcript_for_test;
pub use types::{
    AgentConfig, AgentTurnOutcome, ForegroundAgentWaitRequest, SandboxAgentSpawnRequest,
    TurnWebSearch, UtilityModel, DEFAULT_CONTEXT_WINDOW, DEFAULT_MAX_STEPS,
    DEFAULT_MAX_TOOL_RESULT_BYTES,
};

use std::sync::Arc;

use chrono::Utc;
use serde_json::Value;

use crate::approval::{ApprovalGate, StandingGrants};
use crate::cancel::CancelToken;
use crate::citation::AssistantCitationInput;
use crate::error::ProviderErrorInfo;
use crate::id::{CallId, ChatId, MessageId, TurnId};
use crate::model::{Message, Role, ToolCallRecord};
use crate::preview::ToolActionPreview;
use crate::provider::{
    ChatMessage, ContentBlock, MessageReasoning, ModelProvider, RefusalDetails, StopReason,
};
use crate::steer::SteerInbox;
use crate::storage::{BlobStore, Store};
use crate::tool::ToolOutput;

// `pub use` above brings ToolRegistry and config types into this module.

pub(crate) const MAX_PARALLEL_READ_ONLY_CALLS: usize = 8;
/// How many consecutive identical server calls — same tool, same canonicalized
/// arguments — may execute before the loop steps in. Once a streak reaches
/// this length the next identical call is answered without running: the model
/// has already seen everything the call can tell it, so another repeat is a
/// stuck loop, not new work. Any different call, a plain text step, or a
/// reader decline breaks the streak; the refusal itself leaves it intact, so
/// re-issuing the same call keeps getting the refusal while a changed argument
/// proceeds normally.
pub(crate) const REPEATED_CALL_LIMIT: usize = 3;

pub(crate) struct StreamAttempt {
    end: StreamEnd,
    text: String,
    calls: Vec<PendingCall>,
    /// Tool calls the provider ran server-side, already complete. Never
    /// dispatched — see [`ContentBlock::ProviderExecutedToolCall`].
    provider_executed: Vec<ContentBlock>,
    reasoning: Vec<Value>,
    stop_reason: StopReason,
    refusal_details: Option<RefusalDetails>,
}

pub(crate) enum StreamEnd {
    Done,
    Cancelled,
    Steered,
    Failed(ProviderErrorInfo),
}

/// Appended in model context to the partial prose a cancelled turn committed
/// (#1182). Never stored and never rendered — the durable message and the
/// transcript keep exactly what the user watched stream.
pub(crate) const USER_INTERRUPTION_NOTE: &str = "\n\n[The user stopped this response here]";

#[derive(Clone, Copy)]
pub(crate) struct TurnExecution<'a> {
    turn_id: TurnId,
    user_input: &'a str,
    output_message_id: MessageId,
    persist_input: bool,
    publish_started: bool,
    publish_terminal: bool,
}

/// Drives turns for a chat over a provider, tool set, and store.
pub struct Agent {
    provider: Arc<dyn ModelProvider>,
    tools: Arc<ToolRegistry>,
    store: Arc<dyn Store>,
    blobs: Option<Arc<dyn BlobStore>>,
    config: AgentConfig,
    approvals: Arc<dyn ApprovalGate>,
    standing_grants: Arc<StandingGrants>,
    cancel: CancelToken,
    steer: SteerInbox,
    durable_steer_lease: Option<uuid::Uuid>,
    agent_orchestration_enabled: bool,
    continuation_instruction: Option<String>,
    pending_sandbox_spawns: Vec<SandboxAgentSpawnRequest>,
    pending_sandbox_spawn_steer_revision: Option<i64>,
}

/// A tool call accumulated from the provider stream.
pub(crate) struct PendingCall {
    call_id: CallId,
    provider_id: String,
    name: String,
    args: String,
}

/// The rebuilt provider transcript and the point covered by a durable
/// checkpoint, if that checkpoint's source still exists in the chat.
///
/// The boundary is measured in provider messages, not durable rows, because a
/// provider turn can include reconstructed tool-use/result messages between
/// two stored messages. Soft load assembly skips the covered prefix for the
/// model while the UI transcript stays complete.
pub(crate) struct LoadedTranscript {
    messages: Vec<ChatMessage>,
    checkpoint_boundary: Option<usize>,
    source_boundaries: Vec<TranscriptSourceBoundary>,
    /// Durable user texts for `original_requests` carry-forward.
    user_texts: Vec<(MessageId, String)>,
}

/// Inclusive provider boundary contributed by one durable transcript row.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TranscriptSourceBoundary {
    message_id: MessageId,
    role: Role,
    provider_boundary: usize,
}

/// Why one call in a step's batch cannot run beside its siblings.
///
/// Every variant is a checkpoint: it suspends the turn and carries exactly one
/// call out of the loop, so a batch can honour only one of them. These are
/// runtime constraints, not mistakes the model made — providers parallelise
/// tool calls by design, so the loop orders the batch around them instead of
/// asking the model for a shape it cannot guarantee. Sensitive calls are not
/// isolated: they run in-step, sequentially after the plain siblings, which
/// keeps a parked approval the turn's only pending row without declining
/// anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CallIsolation {
    /// Leaves the loop as a checkpoint the client resumes.
    Client,
    /// Leaves the loop as a sandbox delegation checkpoint.
    SandboxSpawn,
    /// Leaves the loop as an ordered child-wait checkpoint.
    AgentWait,
}

/// What the approval gate decided about one delegation.
pub(crate) enum SandboxSpawnGate {
    /// The spawn may proceed. The request records whether a durable pending
    /// tool-call row is waiting for the checkpoint to finalize.
    Admit(SandboxAgentSpawnRequest),
    /// The spawn will not happen. Its durable row is already terminal and its
    /// `ToolCallCompleted` event has been published; this is what the model
    /// reads.
    Declined(ToolOutput),
}

/// How one client call's model-facing arguments map onto the canonical durable
/// arguments its checkpoint stores.
pub(crate) enum ClientArgumentResolution {
    /// The model's arguments are already the canonical form.
    Unchanged,
    /// The model named a host identity that resolved into canonical arguments.
    Resolved(Value),
    /// The named identity does not exist; the model gets this answer instead
    /// of a checkpoint.
    Refused(String),
}

/// The closed action projection for a pending call, parsed from the arguments
/// it will run with. Arguments that never parsed cannot describe an action.
pub(crate) fn call_action_preview(call: &PendingCall) -> Option<ToolActionPreview> {
    serde_json::from_str(&call.args)
        .ok()
        .and_then(|args| ToolActionPreview::build(&call.name, &args))
}

/// Project the rows of a provider-executed search result into activity entries.
///
/// The adapter normalizes its vendor output to the shape the host tool of the
/// same name produces, so the card the reader sees is built from the same
/// fields whichever route ran the search. Anything that does not carry that
/// shape simply contributes no rows.
pub(crate) fn provider_executed_entries(output: &Value) -> Vec<crate::ResultEntry> {
    output
        .get("results")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .map(|result| {
            let url = result
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let entry = crate::ResultEntry::new(
                crate::ResultEntryKind::Link,
                result.get("title").and_then(Value::as_str).unwrap_or(""),
            )
            .with_web_url(url);
            match result_host(url) {
                Some(host) => entry.with_detail(host),
                None => entry,
            }
        })
        .collect()
}

/// The bare host of a result URL, for the secondary line of its row.
///
/// Deliberately a display projection rather than a parse: nothing routes on the
/// answer, and a URL this cannot read simply shows no host.
pub(crate) fn result_host(url: &str) -> Option<String> {
    let host = url
        .split_once("://")?
        .1
        .split(['/', '?', '#'])
        .next()?
        .rsplit('@')
        .next()?
        .split(':')
        .next()?
        .trim_start_matches("www.");
    (!host.is_empty()).then(|| host.to_owned())
}

pub(crate) struct AssistantCandidate {
    message_id: MessageId,
    content: String,
    citations: Vec<AssistantCitationInput>,
    /// The step's reasoning, bound to the route that produced it. Rides the
    /// durable message so a later turn can replay it to the same model.
    reasoning: MessageReasoning,
}

impl AssistantCandidate {
    /// This candidate as a durable message under `message_id`.
    ///
    fn message(&self, message_id: MessageId, chat_id: ChatId, turn_id: TurnId) -> Message {
        Message {
            id: message_id,
            chat_id,
            turn_id,
            role: Role::Assistant,
            content: self.content.clone(),
            llm_content: None,
            reasoning: self.reasoning.clone(),
            created_at: Utc::now(),
        }
    }
}

pub(crate) enum AcceptedServerCall {
    Accepted,
    Existing(Box<ToolCallRecord>),
    IdentityConflict,
    LeaseLost,
}
