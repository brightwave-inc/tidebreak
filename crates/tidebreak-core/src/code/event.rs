//! Normalized event vocabulary for an external agent-engine session.
//!
//! Follows the chat journal's conventions: internally tagged serde,
//! `#[non_exhaustive]`, bounded payloads. Large bodies (diffs, raw engine
//! payloads) never ride these events — they carry hints.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{AttentionSource, AttentionState, CodeApprovalId, CodeTurnId, HarnessKind};

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
    /// so the detail on [`CodeEvent::ToolStarted`] may name nothing. A later
    /// detail built from the complete arguments rides
    /// [`CodeEvent::ToolCompleted`] and replaces the first one only when it
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default, TS)]
pub struct CodeUsage {
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

/// Bounded error carried on [`CodeEvent::TurnFailed`].
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

/// Decision recorded on [`CodeEvent::ApprovalResolved`].
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
}

/// One event in an external agent-engine session's journal.
///
/// Serialized as an internally-tagged union (a `type` field selects the
/// variant), and `#[non_exhaustive]` so new event kinds can be added without
/// breaking downstream consumers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum CodeEvent {
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
        turn_id: CodeTurnId,
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
        /// Classification rebuilt from the call's complete arguments.
        ///
        /// Engines open a tool call before its arguments finish streaming, so
        /// the detail on [`CodeEvent::ToolStarted`] can name nothing. This is
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
        approval_id: CodeApprovalId,
    },
    /// A parked approval was decided.
    ApprovalResolved {
        /// The approval that was decided.
        approval_id: CodeApprovalId,
        /// The decision.
        decision: ApprovalDecisionKind,
    },
    /// The user injected a mid-turn message.
    UserSteered {
        /// The steered user text, already bounded.
        text: String,
    },
    /// The turn finished successfully.
    TurnCompleted {
        /// Token accounting as reported by the engine.
        usage: CodeUsage,
        /// Checkpoint recorded at turn end, when any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        checkpoint: Option<CheckpointHint>,
    },
    /// The turn failed and was abandoned.
    TurnFailed {
        /// Bounded error.
        error: BoundedError,
    },
    /// The turn was interrupted (user or recovery).
    TurnInterrupted,
    /// A per-turn checkpoint was recorded.
    CheckpointRecorded {
        /// The turn that ended at this checkpoint.
        turn_id: CodeTurnId,
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
    /// Server-computed attention changed.
    AttentionChanged {
        /// New state.
        state: AttentionState,
        /// Who or what set it.
        source: AttentionSource,
    },
}

/// A [`CodeEvent`] paired with its per-session sequence number.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct SequencedCodeEvent {
    /// Monotonic per-session sequence number; the first event is 1.
    pub seq: i64,
    /// The event at this position.
    pub event: CodeEvent,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::{AttentionSource, AttentionState, FenceReason, HarnessKind};
    use uuid::Uuid;

    #[test]
    fn event_is_internally_tagged() {
        let ev = CodeEvent::AssistantDelta { text: "hi".into() };
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["type"], "assistant_delta");
        assert_eq!(json["text"], "hi");
    }

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn variant_index(event: &CodeEvent) -> usize {
        match event {
            CodeEvent::SessionStarted { .. } => 0,
            CodeEvent::TurnStarted { .. } => 1,
            CodeEvent::AssistantDelta { .. } => 2,
            CodeEvent::AssistantMessage { .. } => 3,
            CodeEvent::ReasoningDelta { .. } => 4,
            CodeEvent::ToolStarted { .. } => 5,
            CodeEvent::ToolCompleted { .. } => 6,
            CodeEvent::FileChanged { .. } => 7,
            CodeEvent::ApprovalRequested { .. } => 8,
            CodeEvent::ApprovalResolved { .. } => 9,
            CodeEvent::UserSteered { .. } => 10,
            CodeEvent::TurnCompleted { .. } => 11,
            CodeEvent::TurnFailed { .. } => 12,
            CodeEvent::TurnInterrupted => 13,
            CodeEvent::CheckpointRecorded { .. } => 14,
            CodeEvent::HarnessNotice { .. } => 15,
            CodeEvent::AttentionChanged { .. } => 16,
        }
    }

    fn journal_samples() -> Vec<CodeEvent> {
        vec![
            CodeEvent::SessionStarted {
                harness_kind: HarnessKind::ClaudeCode,
                harness_version: "2.1.233".into(),
                resume_ref: Some("session-ref".into()),
            },
            CodeEvent::TurnStarted {
                turn_id: CodeTurnId(id(1)),
            },
            CodeEvent::AssistantDelta {
                text: "hello".into(),
            },
            CodeEvent::AssistantMessage {
                text: "hello from fixture".into(),
                parent_call_id: None,
            },
            CodeEvent::ReasoningDelta {
                text: "thinking".into(),
            },
            CodeEvent::ToolStarted {
                call_id: "toolu_1".into(),
                name: "Read".into(),
                detail: ToolDetail::FileRead {
                    path: "README.md".into(),
                },
                parent_call_id: None,
            },
            CodeEvent::ToolCompleted {
                call_id: "toolu_1".into(),
                outcome: ToolOutcome::Succeeded,
                preview: "demo".into(),
                detail: Some(ToolDetail::FileRead {
                    path: "README.md".into(),
                }),
                parent_call_id: None,
            },
            CodeEvent::FileChanged {
                path: "src/lib.rs".into(),
                kind: FileChangeKind::Modified,
                diffstat: Diffstat {
                    files: 1,
                    insertions: 3,
                    deletions: 1,
                    truncated: false,
                },
            },
            CodeEvent::ApprovalRequested {
                approval_id: CodeApprovalId(id(2)),
            },
            CodeEvent::ApprovalResolved {
                approval_id: CodeApprovalId(id(2)),
                decision: ApprovalDecisionKind::Deny {
                    feedback: Some("use the fixtures directory".into()),
                },
            },
            CodeEvent::UserSteered {
                text: "try the other file".into(),
            },
            CodeEvent::TurnCompleted {
                usage: CodeUsage {
                    input_tokens: 11,
                    output_tokens: 22,
                    cache_read_input_tokens: 33,
                    cache_creation_input_tokens: 44,
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
            },
            CodeEvent::TurnFailed {
                error: BoundedError {
                    message: "engine exited 1".into(),
                },
            },
            CodeEvent::TurnInterrupted,
            CodeEvent::CheckpointRecorded {
                turn_id: CodeTurnId(id(1)),
                diffstat: Diffstat {
                    files: 2,
                    insertions: 10,
                    deletions: 1,
                    truncated: true,
                },
            },
            CodeEvent::HarnessNotice {
                level: HarnessNoticeLevel::Warning,
                message: "unrecognized event type counted".into(),
            },
            CodeEvent::AttentionChanged {
                state: AttentionState::Fenced {
                    reason: FenceReason::OrphanAlive,
                },
                source: AttentionSource::Lifecycle,
            },
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

    /// `CodeEvent` is written into `code_event.event` and read back, so its
    /// field names are a storage format. Pre-v1 this test is a change
    /// detector: a payload change that ships without a
    /// `DESKTOP_SCHEMA_EPOCH` bump would make old rows unreadable.
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
            "the code journal event shape changed; if this is deliberate, bump \
             DESKTOP_SCHEMA_EPOCH in tidebreak-server so existing databases are \
             discarded, then regenerate with UPDATE_CODE_JOURNAL_FIXTURE=1 cargo \
             test -p tidebreak-core"
        );
    }
}
