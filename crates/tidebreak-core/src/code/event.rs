//! Normalized event vocabulary for one agent-engine session — external
//! harnesses and the internal engine alike (decision 0048 step 5).
//!
//! Internally tagged serde, `#[non_exhaustive]`, bounded payloads. Large
//! bodies (diffs, raw engine payloads) never ride these events — they carry
//! hints. Variants and fields marked *internal engine* are written only by
//! the in-process engine's turn lane, which the server controls: they carry
//! the structured facts the chat surface replays (the alias layer the
//! decision's amendment permits), and external adapters never produce them.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{ApprovalId, ApprovalKind, HarnessKind, TurnId};
use crate::approval::{GrantScope, ToolApprovalKind};
use crate::error::AgentErrorInfo;
use crate::preview::{ToolActionPreview, ToolResultPreview, MAX_ACTION_FIELD_CHARS};
use crate::provider::{RefusalOutcome, StopReason};
use crate::tool::{ApprovalClass, ToolOutput};

/// Longest assistant / reasoning / steer text stored on one event.
pub const MAX_EVENT_TEXT_CHARS: usize = 8_192;
/// Longest tool-result preview.
pub const MAX_PREVIEW_CHARS: usize = 2_048;
/// Longest harness-notice message.
pub const MAX_NOTICE_CHARS: usize = 1_024;
/// Longest tool-detail summary.
pub const MAX_TOOL_SUMMARY_CHARS: usize = 512;

/// Display-oriented classification of a tool the engine started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolDetail {
    /// A shell command.
    Command {
        /// Command string.
        cmd: String,
        /// Working directory, when reported.
        cwd: String,
    },
    /// A file edit.
    FileEdit {
        /// Path being edited.
        path: String,
    },
    /// A file read.
    FileRead {
        /// Path being read.
        path: String,
    },
    /// A search.
    Search {
        /// Query string.
        query: String,
    },
    /// Anything else, with a short summary.
    Other {
        /// Bounded summary.
        summary: String,
    },
}

impl ToolDetail {
    /// The subject a transcript line names: the command, path, or query.
    #[must_use]
    pub fn subject(&self) -> &str {
        match self {
            Self::Command { cmd, .. } => cmd,
            Self::FileEdit { path } | Self::FileRead { path } => path,
            Self::Search { query } => query,
            Self::Other { summary } => summary,
        }
    }

    /// How much this detail says about the call, for the correction channel.
    ///
    /// An engine can open a tool call before its arguments finish streaming,
    /// so the detail on [`Event::ToolStarted`] may name nothing. A later
    /// detail built from the complete arguments rides
    /// [`Event::ToolCompleted`] and replaces the first one only when it
    /// scores higher, so a correction never downgrades a line that already
    /// names its subject.
    ///
    /// Zero is a detail with no subject, one is a bare tool name, and two is
    /// a real command, path, or query.
    #[must_use]
    pub fn specificity(&self) -> u8 {
        if self.subject().trim().is_empty() {
            0
        } else if matches!(self, Self::Other { .. }) {
            1
        } else {
            2
        }
    }
}

/// How a tool call finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ToolOutcome {
    /// The tool ran and returned a result.
    Succeeded,
    /// The tool reported an error.
    Failed,
    /// The engine or user denied the call.
    Denied,
}

/// Kind of file change reported by the engine or a checkpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum FileChangeKind {
    /// A new file.
    Added,
    /// An existing file changed.
    Modified,
    /// A file was removed.
    Deleted,
    /// A file was renamed.
    Renamed,
}

/// Bounded add/delete counts for a diff. Bodies live on a GET route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Diffstat {
    /// Files touched.
    pub files: u32,
    /// Lines added.
    pub insertions: u32,
    /// Lines deleted.
    pub deletions: u32,
    /// True when the underlying diff was truncated.
    #[serde(default)]
    pub truncated: bool,
}

/// Token accounting for one turn.
///
/// The four counts are **disjoint** and they are **turn totals**, summed over
/// every model call the engine made while servicing the turn. This is the same
/// contract the chat side states on `RendererTurnUsage`, and the reason to
/// state it here is that every adapter reports something different natively:
/// one engine sends a running total beside the last call's slice, another
/// folds the cached portion back into the prompt count, another overwrites a
/// snapshot per message. Normalizing belongs in the adapter, so that anything
/// reading this struct — cost accounting, the CLI's turn list, the desktop's
/// context indicator — can compare two harnesses without knowing which engine
/// produced the row.
///
/// Concretely: `input_tokens` is the *fresh*, uncached prompt only. It never
/// includes `cache_read_input_tokens` or `cache_creation_input_tokens`, so
/// the prompt an engine actually sent is the sum of all three. Missing fields
/// stay zero, which is not the same as "the engine sent zero" — an engine that
/// does not surface cache counts reports nothing rather than a real zero.
///
/// None of the four answers "how full is the window". Summing turn totals
/// counts the same transcript once per model call, so a long turn reads as a
/// multiple of the prompt that was actually resident. That reading has its own
/// field: [`TurnUsage::context_tokens`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default, TS)]
pub struct TurnUsage {
    /// Fresh, uncached input tokens. Excludes both cache fields.
    #[serde(default)]
    pub input_tokens: u64,
    /// Output tokens, summed over the turn's model calls.
    #[serde(default)]
    pub output_tokens: u64,
    /// Cache-read input tokens, when the engine reports them.
    #[serde(default)]
    pub cache_read_input_tokens: u64,
    /// Cache-write input tokens, when the engine reports them.
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    /// Prompt tokens resident on the turn's final model call — what actually
    /// occupied the context window at the end of the turn.
    ///
    /// Distinct from the four counts above, which are the turn's *spend*
    /// summed across every model call. On a six-call turn those sum to
    /// roughly six prompts; this is the one prompt that was live when the
    /// turn ended, and it is the only honest numerator for "how full is the
    /// window".
    ///
    /// Zero when the engine does not publish enough to compute it.
    #[serde(default)]
    pub context_tokens: u64,
    /// Prompt tokens resident on the turn's first model call, when the engine
    /// publishes per-call usage. This exposes startup context separately from
    /// context that grows while the turn runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub first_call_context_tokens: Option<u64>,
}

/// Hint that a turn recorded a checkpoint. The diff body is loaded separately.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct CheckpointHint {
    /// Hidden ref name, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub checkpoint_ref: Option<String>,
    /// Bounded diffstat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub diffstat: Option<Diffstat>,
}

/// Bounded error carried on [`Event::TurnFailed`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct BoundedError {
    /// Short message, already truncated by the adapter.
    pub message: String,
}

/// Severity of a visible-degradation notice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum HarnessNoticeLevel {
    /// Informational.
    Info,
    /// Something degraded but the turn continues.
    Warning,
    /// The engine reported an error that is not turn-fatal on its own.
    Error,
}

/// Why a machine session's own git or `gh` was refused a forge credential
/// at the loopback route (decision 63's seam), so the surfaces can name
/// the remedy rather than show the bare status git prints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum CredentialRefusalReason {
    /// The external connection that authorizes this session is gone:
    /// revoked, replaced by a newer connect, or no longer delegated by the
    /// gateway. Reconnecting from the channel is the remedy.
    ConnectionEnded,
    /// The forge offers no identity for this session yet: the person has
    /// not connected their account at the gateway.
    NotConnected,
    /// The forge or the gateway refused the mint for another reason, which
    /// the message carries.
    ForgeRefused,
}

/// Outcome recorded on [`Event::ApprovalResolved`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ApprovalDecisionKind {
    /// Approved.
    Approve,
    /// Denied, optionally with steering feedback.
    Deny {
        /// Feedback returned to the engine, when any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        feedback: Option<String>,
    },
    /// Nobody decided: the tool call resolved first, so the request is dead.
    ///
    /// This rides `ApprovalResolved` rather than an event of its own so that
    /// every consumer already listening for "this approval is settled" stops
    /// showing the request. A separate event would let an un-updated reader
    /// keep rendering a pending card that can never be acted on.
    Abandoned,
    /// Approved, and a standing grant was minted at this scope.
    ///
    /// Only engines declaring `standing_grants` resolve an approval this way
    /// (decision 0048: external harnesses keep no standing grants).
    ApprovedWithGrant {
        /// The scope the decider granted.
        scope: crate::GrantScope,
    },
    /// A questions approval was answered.
    Answered {
        /// The supplied answers, already validated and bounded.
        answers: Vec<crate::UserQuestionAnswer>,
    },
    /// A plan approval was decided.
    PlanDecided {
        /// Whether the plan was accepted.
        approve: bool,
        /// Feedback returned to the engine, when any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        feedback: Option<String>,
    },
}

/// What an internal-engine approval row asks, journaled beside its id so
/// the chat surface replays the card without loading the row. Internal
/// engine: external adapters carry only the row id, and the row is the
/// source of truth for every reader.
///
/// The row's id is the engine call id the card is parked on (one approval
/// surface, decision 0048 step 5), so the chat surface recovers the call
/// from the approval id alone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InternalApprovalRequest {
    /// A tool call needs the user's consent before it runs, in the internal
    /// engine's own terms: the class that triggered the prompt, the approval
    /// kind, the standing-grant ladder this exact call cleared, and the
    /// closed preview of what the call will do.
    ToolUse {
        /// Whether the Auto-mode judge owns this card right now.
        #[serde(default, skip_serializing_if = "is_false")]
        auto_judging: bool,
        /// Canonical registered tool identity.
        tool_name: String,
        /// The approval class that triggered the prompt.
        class: ApprovalClass,
        /// What kind of consent the call asks for.
        approval: ToolApprovalKind,
        /// Every standing-grant rung the call cleared; empty means approving
        /// once is the only affirmative choice.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        grant_scopes: Vec<GrantScope>,
        /// Closed projection of what the call will do, when its tool has one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        preview: Option<ToolActionPreview>,
        /// True when the preview was cut to the action-field bound so a
        /// channel adapter can fall back to a web link for the rest.
        #[serde(default, skip_serializing_if = "is_false")]
        preview_truncated: bool,
    },
    /// A questions card parked the turn; the questions ride the row's kind.
    Questions {
        /// Turn that resumes after the answer commits.
        turn_id: TurnId,
    },
    /// A plan proposal parked the turn; the plan body rides the row.
    Plan {
        /// Turn that resumes after the decision commits.
        turn_id: TurnId,
    },
}

impl InternalApprovalRequest {
    /// Card facts a channel can render without loading the approval row.
    #[must_use]
    pub fn from_kind(kind: &ApprovalKind, turn_id: TurnId, auto_judging: bool) -> Self {
        match kind {
            ApprovalKind::ToolUse {
                preview,
                offered_grants,
            } => {
                let (preview, preview_truncated) = bound_action_preview(preview.clone());
                Self::ToolUse {
                    auto_judging,
                    tool_name: tool_name_for_preview(&preview),
                    class: class_for_preview(&preview),
                    approval: ToolApprovalKind::for_tool_name(&tool_name_for_preview(&preview)),
                    grant_scopes: offered_grants.clone(),
                    preview: Some(preview),
                    preview_truncated,
                }
            }
            ApprovalKind::Questions { .. } => Self::Questions { turn_id },
            ApprovalKind::Plan { .. } => Self::Plan { turn_id },
            ApprovalKind::Command { cmd, cwd } => {
                let (command, preview_truncated) = bound_preview_text(cmd);
                Self::ToolUse {
                    auto_judging,
                    tool_name: "exec".into(),
                    class: ApprovalClass::Workspace,
                    approval: ToolApprovalKind::ExecMayRunNetworkedCommand,
                    grant_scopes: Vec::new(),
                    preview: Some(ToolActionPreview::Exec {
                        command,
                        args: Vec::new(),
                        cwd: cwd.clone().unwrap_or_else(|| ".".into()),
                        files: Vec::new(),
                        summary: None,
                    }),
                    preview_truncated,
                }
            }
            ApprovalKind::FileWrite { paths } => {
                // The person consents to every path, so the card names them
                // all: the first as the path, the whole set in the summary.
                // A set the summary cannot hold is a truncated preview, and
                // the adapter falls back to the web link.
                let path = paths.first().cloned().unwrap_or_default();
                let (path, path_truncated) = bound_preview_text(&path);
                let (summary, summary_truncated) = if paths.len() > 1 {
                    let (joined, truncated) = bound_preview_text(&paths.join("\n"));
                    (Some(format!("{} files:\n{joined}", paths.len())), truncated)
                } else {
                    (None, false)
                };
                Self::ToolUse {
                    auto_judging,
                    tool_name: "write_file".into(),
                    class: ApprovalClass::Workspace,
                    approval: ToolApprovalKind::WorkspaceMayModifyFiles,
                    grant_scopes: Vec::new(),
                    preview: Some(ToolActionPreview::WriteFile { path, summary }),
                    preview_truncated: path_truncated || summary_truncated,
                }
            }
            ApprovalKind::Network { summary } | ApprovalKind::Other { summary } => {
                let (summary, preview_truncated) = bound_preview_text(summary);
                Self::ToolUse {
                    auto_judging,
                    tool_name: "other".into(),
                    class: ApprovalClass::Sensitive,
                    approval: ToolApprovalKind::Unsupported,
                    grant_scopes: Vec::new(),
                    preview: Some(ToolActionPreview::WriteFile {
                        path: summary,
                        summary: None,
                    }),
                    preview_truncated,
                }
            }
        }
    }
}

fn bound_preview_text(text: &str) -> (String, bool) {
    if text.chars().count() > MAX_ACTION_FIELD_CHARS {
        (text.chars().take(MAX_ACTION_FIELD_CHARS).collect(), true)
    } else {
        (text.to_owned(), false)
    }
}

/// Whether a field the preview builder already clamped may have been cut.
///
/// `ToolActionPreview::build` clamps every field to `MAX_ACTION_FIELD_CHARS`
/// and keeps no record of it, so by the time a card is projected the only
/// trace of a cut is a field sitting exactly at the cap. Treating that as
/// truncated is conservative: a field that happens to be exactly the cap's
/// length reads as possibly cut, and the adapter falls back to the web link,
/// which is the safe direction for a consent surface.
fn at_field_cap(text: &str) -> bool {
    text.chars().count() >= MAX_ACTION_FIELD_CHARS
}

fn any_at_field_cap<'a>(fields: impl IntoIterator<Item = &'a str>) -> bool {
    fields.into_iter().any(at_field_cap)
}

fn bound_action_preview(preview: ToolActionPreview) -> (ToolActionPreview, bool) {
    let already_cut = match &preview {
        ToolActionPreview::Exec {
            command,
            args,
            cwd,
            files,
            summary,
        } => any_at_field_cap(
            [command.as_str(), cwd.as_str()]
                .into_iter()
                .chain(args.iter().map(String::as_str))
                .chain(files.iter().map(String::as_str))
                .chain(summary.as_deref()),
        ),
        ToolActionPreview::Search { query, summary } => {
            any_at_field_cap([query.as_str()].into_iter().chain(summary.as_deref()))
        }
        ToolActionPreview::WebSearch {
            query,
            domains,
            start_published_at,
            end_published_at,
            summary,
        } => any_at_field_cap(
            [query.as_str()]
                .into_iter()
                .chain(domains.iter().map(String::as_str))
                .chain(start_published_at.as_deref())
                .chain(end_published_at.as_deref())
                .chain(summary.as_deref()),
        ),
        ToolActionPreview::WebExtract { url, summary } => {
            any_at_field_cap([url.as_str()].into_iter().chain(summary.as_deref()))
        }
        ToolActionPreview::WriteFile { path, summary } => {
            any_at_field_cap([path.as_str()].into_iter().chain(summary.as_deref()))
        }
        ToolActionPreview::DelegateAgent { .. } => false,
    };
    let (preview, truncated) = match preview {
        ToolActionPreview::Exec {
            command,
            args,
            cwd,
            files,
            summary,
        } => {
            let (command, truncated) = bound_preview_text(&command);
            (
                ToolActionPreview::Exec {
                    command,
                    args,
                    cwd,
                    files,
                    summary,
                },
                truncated,
            )
        }
        ToolActionPreview::Search { query, summary } => {
            let (query, truncated) = bound_preview_text(&query);
            (ToolActionPreview::Search { query, summary }, truncated)
        }
        ToolActionPreview::WebSearch {
            query,
            domains,
            start_published_at,
            end_published_at,
            summary,
        } => {
            let (query, truncated) = bound_preview_text(&query);
            (
                ToolActionPreview::WebSearch {
                    query,
                    domains,
                    start_published_at,
                    end_published_at,
                    summary,
                },
                truncated,
            )
        }
        ToolActionPreview::WebExtract { url, summary } => {
            let (url, truncated) = bound_preview_text(&url);
            (ToolActionPreview::WebExtract { url, summary }, truncated)
        }
        ToolActionPreview::WriteFile { path, summary } => {
            let (path, truncated) = bound_preview_text(&path);
            (ToolActionPreview::WriteFile { path, summary }, truncated)
        }
        other => (other, false),
    };
    (preview, truncated || already_cut)
}

fn tool_name_for_preview(preview: &ToolActionPreview) -> String {
    match preview {
        ToolActionPreview::Exec { .. } => "exec".into(),
        ToolActionPreview::Search { .. } => "search".into(),
        ToolActionPreview::WebSearch { .. } => "web_search".into(),
        ToolActionPreview::WebExtract { .. } => "web_extract".into(),
        ToolActionPreview::WriteFile { .. } => "write_file".into(),
        ToolActionPreview::DelegateAgent { .. } => crate::SPAWN_SANDBOX_AGENT_TOOL.into(),
    }
}

fn class_for_preview(preview: &ToolActionPreview) -> ApprovalClass {
    match preview {
        ToolActionPreview::Exec { .. }
        | ToolActionPreview::WriteFile { .. }
        | ToolActionPreview::DelegateAgent { .. } => ApprovalClass::Workspace,
        ToolActionPreview::Search { .. }
        | ToolActionPreview::WebSearch { .. }
        | ToolActionPreview::WebExtract { .. } => ApprovalClass::Sensitive,
    }
}

/// One event in an external agent-engine session's journal.
///
/// Serialized as an internally-tagged union (a `type` field selects the
/// variant), and `#[non_exhaustive]` so new event kinds can be added without
/// breaking downstream consumers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Event {
    /// The engine session has started (or resumed).
    SessionStarted {
        /// Which engine.
        harness_kind: HarnessKind,
        /// Version observed at launch.
        harness_version: String,
        /// Engine-native resume token, when the stream reported one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        resume_ref: Option<String>,
    },
    /// A user→engine turn has begun.
    TurnStarted {
        /// The turn being processed.
        turn_id: TurnId,
    },
    /// You continue a parked turn after a worker restart rather than
    /// starting over. The engine's durable checkpoint is still the same turn.
    TurnResumed {
        /// The turn that continues.
        turn_id: TurnId,
    },
    /// A chunk of assistant text.
    AssistantDelta {
        /// The text fragment to append.
        text: String,
    },
    /// A completed assistant message (whole text, already bounded).
    AssistantMessage {
        /// The message text.
        text: String,
        /// The `Task` call this message ran inside, when a harness subagent
        /// produced it (decision 52). Absent on the parent's own messages.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        parent_call_id: Option<String>,
    },
    /// A chunk of reasoning/thinking text, where the engine reports it.
    ReasoningDelta {
        /// The reasoning fragment.
        text: String,
    },
    /// The engine has begun a tool call.
    ToolStarted {
        /// Engine-native call id.
        call_id: String,
        /// Tool name as the engine reported it.
        name: String,
        /// Display-oriented classification.
        detail: ToolDetail,
        /// The `Task` call this call ran inside, when a harness subagent
        /// issued it (decision 52). Absent on the parent's own calls.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        parent_call_id: Option<String>,
    },
    /// A tool call finished.
    ToolCompleted {
        /// Engine-native call id.
        call_id: String,
        /// How it finished.
        outcome: ToolOutcome,
        /// Bounded preview of the result.
        preview: String,
        /// The call's whole output, already cut to what the model was shown.
        ///
        /// Internal engine: its tools run in this process, so the server
        /// holds the result and the chat surface replays it. External
        /// adapters leave it unset; their results ride `preview`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        output: Option<Box<ToolOutput>>,
        /// Closed projection of what the call did. Internal engine.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        action: Option<ToolActionPreview>,
        /// Closed projection of what the call produced. Internal engine.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        result: Option<ToolResultPreview>,
        /// Classification rebuilt from the call's complete arguments.
        ///
        /// Engines open a tool call before its arguments finish streaming, so
        /// the detail on [`Event::ToolStarted`] can name nothing. This is
        /// the correction: adapters that see the final arguments fill it in,
        /// and renderers merge it into the started call. It is `None` when
        /// the engine's completion payload carries no arguments.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        detail: Option<ToolDetail>,
        /// The `Task` call this call ran inside, when a harness subagent
        /// issued it (decision 52). Absent on the parent's own calls.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        parent_call_id: Option<String>,
    },
    /// A file changed. The diff body is loaded from a bounded GET route.
    FileChanged {
        /// Path relative to the worktree, when the engine reports one.
        path: String,
        /// Kind of change.
        kind: FileChangeKind,
        /// Bounded diffstat.
        diffstat: Diffstat,
    },
    /// An approval is waiting. The body loads from the approvals route.
    ApprovalRequested {
        /// Hint id; the row is the source of truth.
        approval_id: ApprovalId,
        /// What the card asks: tool name, class, consent kind, grant ladder,
        /// and a size-capped preview. Written for machine sessions; absent
        /// on sandbox sessions, which never park on these cards.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        request: Option<InternalApprovalRequest>,
    },
    /// A parked approval was decided.
    ApprovalResolved {
        /// The approval that was decided.
        approval_id: ApprovalId,
        /// The decision.
        decision: ApprovalDecisionKind,
        /// Person or automation that made the decision.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        actor: Option<super::TurnActor>,
    },
    /// The user injected a mid-turn message.
    UserSteered {
        /// The steered user text, already bounded.
        text: String,
        /// Stable id of the persisted user message, so a reconnecting
        /// renderer reconciles the event with the transcript row without
        /// content matching. Internal engine.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        message_id: Option<uuid::Uuid>,
    },
    /// The turn finished successfully.
    TurnCompleted {
        /// Token accounting as reported by the engine.
        usage: TurnUsage,
        /// Checkpoint recorded at turn end, when any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        checkpoint: Option<CheckpointHint>,
        /// Why the final model call stopped. Internal engine.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        stop_reason: Option<StopReason>,
    },
    /// The turn failed and was abandoned.
    TurnFailed {
        /// Bounded error.
        error: BoundedError,
        /// The failure's machine-readable kind beside its message, so the
        /// renderer can categorize it. Internal engine.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        detail: Option<AgentErrorInfo>,
    },
    /// The turn was interrupted (user or recovery).
    TurnInterrupted {
        /// Token accounting up to the interruption, when the engine reports
        /// it. Internal engine.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        usage: Option<TurnUsage>,
    },
    /// The turn completed with a model refusal rather than a complete
    /// answer. Internal engine.
    TurnRefused {
        /// Token accounting up to the refusal.
        usage: TurnUsage,
        /// Category detail and whether visible output is incomplete.
        refusal: RefusalOutcome,
    },
    /// A per-turn checkpoint was recorded.
    CheckpointRecorded {
        /// The turn that ended at this checkpoint.
        turn_id: TurnId,
        /// Bounded diffstat.
        diffstat: Diffstat,
    },
    /// Visible degradation or an engine-native notice.
    HarnessNotice {
        /// Severity.
        level: HarnessNoticeLevel,
        /// Bounded message.
        message: String,
    },
    /// The session's own git or `gh` asked the machine for a forge
    /// credential and was refused. Journaled by the loopback route so the
    /// desktop and the channel see why a push stopped, beside the bare
    /// status the helper printed for the engine.
    CredentialRefused {
        /// Which class of refusal, so a renderer can pick the remedy.
        reason: CredentialRefusalReason,
        /// Bounded message, as the machine answered the helper.
        message: String,
        /// Bounded remedy sentence for the person.
        remediation: String,
    },
    /// The provider stream was preempted; clients discard the assistant and
    /// tool deltas streamed since the last stable boundary. Internal engine.
    StreamInterrupted,
    /// A fragment of a tool call's JSON arguments. Internal engine: external
    /// adapters open a call only once its arguments are complete.
    ToolArgsDelta {
        /// The call these args belong to.
        call_id: String,
        /// Partial JSON to concatenate.
        fragment: String,
    },
    /// The engine replaced the session's task plan. A bounded refresh hint:
    /// the steps load from the task-plan route, so a plan rewritten twenty
    /// times does not journal twenty copies. Internal engine.
    TaskPlanUpdated {
        /// The tool call that committed the replacement.
        call_id: String,
        /// Turn that made the call.
        turn_id: TurnId,
    },
    /// The transcript was cut to fit the model's context window before a
    /// model call; the turn continues with reduced context. Internal engine.
    ContextTruncated {
        /// Estimated tokens of the full transcript before reduction.
        original_tokens: u32,
        /// Estimated tokens after fitting to the budget.
        fitted_tokens: u32,
    },
    /// Semantic compaction is about to run. Internal engine.
    CompactionStarted,
    /// Semantic compaction finished for this attempt. Internal engine.
    CompactionFinished {
        /// Whether a new (or confirmed) checkpoint was stored.
        compacted: bool,
    },
}

/// Whether to omit a defaulted `false` flag from a journal row.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(value: &bool) -> bool {
    !*value
}

/// A [`Event`] paired with its per-session sequence number.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct SequencedEvent {
    /// Monotonic per-session sequence number; the first event is 1.
    pub seq: i64,
    /// The event at this position.
    pub event: Event,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::HarnessKind;
    use uuid::Uuid;

    #[test]
    fn a_command_the_builder_clamped_projects_as_a_truncated_card() {
        let turn_id = TurnId::new();
        let long = crate::ToolActionPreview::build(
            "exec",
            &serde_json::json!({ "command": "x".repeat(600) }),
        )
        .expect("a command builds a preview");
        let card = InternalApprovalRequest::from_kind(
            &ApprovalKind::ToolUse {
                preview: long,
                offered_grants: Vec::new(),
            },
            turn_id,
            false,
        );
        let InternalApprovalRequest::ToolUse {
            preview_truncated, ..
        } = card
        else {
            panic!("a tool use projects a tool-use card");
        };
        assert!(
            preview_truncated,
            "a command the builder cut to the cap is not the command being approved"
        );

        let short = crate::ToolActionPreview::build(
            "exec",
            &serde_json::json!({ "command": "cargo test" }),
        )
        .expect("a command builds a preview");
        let card = InternalApprovalRequest::from_kind(
            &ApprovalKind::ToolUse {
                preview: short,
                offered_grants: Vec::new(),
            },
            turn_id,
            false,
        );
        let InternalApprovalRequest::ToolUse {
            preview_truncated, ..
        } = card
        else {
            panic!("a tool use projects a tool-use card");
        };
        assert!(!preview_truncated, "a short command is shown whole");
    }

    #[test]
    fn a_multi_path_file_write_names_every_path_or_says_it_was_cut() {
        let turn_id = TurnId::new();
        let paths = vec!["a.rs".to_owned(), "b.rs".to_owned(), "c.rs".to_owned()];
        let card =
            InternalApprovalRequest::from_kind(&ApprovalKind::FileWrite { paths }, turn_id, false);
        let InternalApprovalRequest::ToolUse {
            preview: Some(ToolActionPreview::WriteFile { path, summary }),
            preview_truncated,
            ..
        } = card
        else {
            panic!("a file write projects a write_file card");
        };
        assert_eq!(path, "a.rs");
        assert_eq!(summary.as_deref(), Some("3 files:\na.rs\nb.rs\nc.rs"));
        assert!(!preview_truncated, "three short paths fit the card");

        let paths: Vec<String> = (0..64)
            .map(|index| format!("{}/{index}.rs", "d".repeat(40)))
            .collect();
        let card =
            InternalApprovalRequest::from_kind(&ApprovalKind::FileWrite { paths }, turn_id, false);
        let InternalApprovalRequest::ToolUse {
            preview_truncated, ..
        } = card
        else {
            panic!("a file write projects a write_file card");
        };
        assert!(
            preview_truncated,
            "a set the summary cannot hold is a truncated preview"
        );
    }

    #[test]
    fn event_is_internally_tagged() {
        let ev = Event::AssistantDelta { text: "hi".into() };
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["type"], "assistant_delta");
        assert_eq!(json["text"], "hi");
    }

    /// The structured resolutions added for the internal engine (decision
    /// 0048 step 5) are a storage format like every other decision kind:
    /// pin their tags and round-trip them.
    #[test]
    fn structured_decision_kinds_round_trip() {
        let kinds = [
            ApprovalDecisionKind::ApprovedWithGrant {
                scope: crate::GrantScope::WholeTool,
            },
            ApprovalDecisionKind::Answered {
                answers: vec![crate::UserQuestionAnswer {
                    question_id: "q1".into(),
                    selected_option_ids: vec!["a".into()],
                    custom_answer: None,
                }],
            },
            ApprovalDecisionKind::PlanDecided {
                approve: true,
                feedback: None,
            },
        ];
        let tags = ["approved_with_grant", "answered", "plan_decided"];
        for (kind, tag) in kinds.iter().zip(tags) {
            let json = serde_json::to_value(kind).unwrap();
            assert_eq!(json["type"], tag);
            let back: ApprovalDecisionKind = serde_json::from_value(json).unwrap();
            assert_eq!(&back, kind);
        }
    }

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn variant_index(event: &Event) -> usize {
        match event {
            Event::SessionStarted { .. } => 0,
            Event::TurnStarted { .. } => 1,
            Event::TurnResumed { .. } => 2,
            Event::AssistantDelta { .. } => 3,
            Event::AssistantMessage { .. } => 4,
            Event::ReasoningDelta { .. } => 5,
            Event::ToolStarted { .. } => 6,
            Event::ToolCompleted { .. } => 7,
            Event::FileChanged { .. } => 8,
            Event::ApprovalRequested { .. } => 9,
            Event::ApprovalResolved { .. } => 10,
            Event::UserSteered { .. } => 11,
            Event::TurnCompleted { .. } => 12,
            Event::TurnFailed { .. } => 13,
            Event::TurnInterrupted { .. } => 14,
            Event::CheckpointRecorded { .. } => 15,
            Event::HarnessNotice { .. } => 16,
            Event::TurnRefused { .. } => 17,
            Event::StreamInterrupted => 18,
            Event::ToolArgsDelta { .. } => 19,
            Event::TaskPlanUpdated { .. } => 20,
            Event::ContextTruncated { .. } => 21,
            Event::CompactionStarted => 22,
            Event::CompactionFinished { .. } => 23,
            Event::CredentialRefused { .. } => 24,
        }
    }

    fn journal_samples() -> Vec<Event> {
        vec![
            Event::SessionStarted {
                harness_kind: HarnessKind::ClaudeCode,
                harness_version: "2.1.233".into(),
                resume_ref: Some("session-ref".into()),
            },
            Event::TurnStarted {
                turn_id: TurnId(id(1)),
            },
            Event::TurnResumed {
                turn_id: TurnId(id(1)),
            },
            Event::AssistantDelta {
                text: "hello".into(),
            },
            Event::AssistantMessage {
                text: "hello from fixture".into(),
                parent_call_id: None,
            },
            Event::ReasoningDelta {
                text: "thinking".into(),
            },
            Event::ToolStarted {
                call_id: "toolu_1".into(),
                name: "Read".into(),
                detail: ToolDetail::FileRead {
                    path: "README.md".into(),
                },
                parent_call_id: None,
            },
            Event::ToolCompleted {
                call_id: "toolu_1".into(),
                outcome: ToolOutcome::Succeeded,
                preview: "demo".into(),
                output: None,
                action: None,
                result: None,
                detail: Some(ToolDetail::FileRead {
                    path: "README.md".into(),
                }),
                parent_call_id: None,
            },
            Event::FileChanged {
                path: "src/lib.rs".into(),
                kind: FileChangeKind::Modified,
                diffstat: Diffstat {
                    files: 1,
                    insertions: 3,
                    deletions: 1,
                    truncated: false,
                },
            },
            Event::ApprovalRequested {
                approval_id: ApprovalId(id(2)),
                request: None,
            },
            Event::ApprovalResolved {
                approval_id: ApprovalId(id(2)),
                decision: ApprovalDecisionKind::Deny {
                    feedback: Some("use the fixtures directory".into()),
                },
                actor: None,
            },
            Event::UserSteered {
                text: "try the other file".into(),
                message_id: None,
            },
            Event::TurnCompleted {
                usage: TurnUsage {
                    input_tokens: 11,
                    output_tokens: 22,
                    cache_read_input_tokens: 33,
                    cache_creation_input_tokens: 44,
                    context_tokens: 88,
                    first_call_context_tokens: Some(55),
                },
                checkpoint: Some(CheckpointHint {
                    checkpoint_ref: Some("refs/tidebreak/checkpoints/ws/1".into()),
                    diffstat: Some(Diffstat {
                        files: 2,
                        insertions: 10,
                        deletions: 1,
                        truncated: false,
                    }),
                }),
                stop_reason: None,
            },
            Event::TurnFailed {
                error: BoundedError {
                    message: "engine exited 1".into(),
                },
                detail: None,
            },
            Event::TurnInterrupted { usage: None },
            Event::CheckpointRecorded {
                turn_id: TurnId(id(1)),
                diffstat: Diffstat {
                    files: 2,
                    insertions: 10,
                    deletions: 1,
                    truncated: true,
                },
            },
            Event::HarnessNotice {
                level: HarnessNoticeLevel::Warning,
                message: "unrecognized event type counted".into(),
            },
            Event::CredentialRefused {
                reason: CredentialRefusalReason::ConnectionEnded,
                message: "this external connection has no live gateway delegation".into(),
                remediation: "Reconnect this session from Slack.".into(),
            },
            // The internal engine's rows. Their optional fields are pinned
            // on the chat journal fixture, which round-trips through these
            // variants; the samples here pin the tags and required fields.
            Event::TurnRefused {
                usage: TurnUsage {
                    input_tokens: 12,
                    output_tokens: 3,
                    cache_read_input_tokens: 0,
                    cache_creation_input_tokens: 0,
                    context_tokens: 0,
                    first_call_context_tokens: None,
                },
                refusal: RefusalOutcome::report_blocked(),
            },
            Event::StreamInterrupted,
            Event::ToolArgsDelta {
                call_id: id(3).to_string(),
                fragment: "{\"command\":".into(),
            },
            Event::TaskPlanUpdated {
                call_id: id(6).to_string(),
                turn_id: TurnId(id(1)),
            },
            Event::ContextTruncated {
                original_tokens: 100_000,
                fitted_tokens: 60_000,
            },
            Event::CompactionStarted,
            Event::CompactionFinished { compacted: true },
        ]
    }

    #[test]
    fn every_journal_event_kind_has_a_sample() {
        let samples = journal_samples();
        let covered: std::collections::BTreeSet<usize> =
            samples.iter().map(variant_index).collect();
        assert_eq!(
            covered.len(),
            samples.len(),
            "two samples describe the same variant"
        );
        assert_eq!(
            covered,
            (0..samples.len()).collect(),
            "a variant is missing from journal_samples"
        );
    }

    const JOURNAL_FIXTURE: &str = "fixtures/code-journal-events.json";

    /// `Event` is written into `event.event` and read back, so its
    /// field names are a storage format. Rows in the old shape survive a
    /// schema change now, so a diff here needs a `#[serde(alias)]` or a
    /// migration, not a refreshed fixture on its own. The chat journal's own
    /// copy of this test says the same thing.
    ///
    /// Regenerate with
    /// `UPDATE_CODE_JOURNAL_FIXTURE=1 cargo test -p tidebreak-core`.
    #[test]
    fn the_journal_event_shape_is_pinned() {
        let rendered = format!(
            "{}\n",
            serde_json::to_string_pretty(&journal_samples()).expect("events serialize")
        );
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(JOURNAL_FIXTURE);
        if std::env::var_os("UPDATE_CODE_JOURNAL_FIXTURE").is_some() {
            std::fs::create_dir_all(path.parent().expect("the fixture path has a parent"))
                .expect("the fixture directory is creatable");
            std::fs::write(&path, &rendered).expect("the fixture path is writable");
            return;
        }
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        assert_eq!(
            existing, rendered,
            "the code journal event shape changed; rows written in the old shape \
             now survive a schema change, so make the new shape readable from \
             the old one with #[serde(alias)], or migrate the rows. Then \
             regenerate with UPDATE_CODE_JOURNAL_FIXTURE=1 cargo test -p \
             tidebreak-core"
        );
    }
}
