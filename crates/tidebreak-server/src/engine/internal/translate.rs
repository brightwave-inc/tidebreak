//! Chat journal vocabulary → adapter vocabulary.
//!
//! The chat turn lane journals [`AgentEvent`]s; the session worker persists
//! [`HarnessEvent`]s. This module is the total mapping between them, kept
//! free of I/O so the shape can be pinned by tests. Events that need a store
//! read to translate — a questions card, a plan proposal — are answered
//! with [`Translated::Lookup`] and finished by the session.

use tidebreak_core::{
    AgentErrorInfo, ApprovalDecision as ChatDecision, BoundedError, CallId, CodeApprovalKind,
    CodeUsage, GrantScope, HarnessNoticeLevel, RefusalOutcome, ToolActionPreview, ToolDetail,
    ToolErrorCategory, ToolOutcome, ToolOutput, Usage, MAX_EVENT_TEXT_CHARS, MAX_NOTICE_CHARS,
    MAX_PREVIEW_CHARS, MAX_TOOL_SUMMARY_CHARS,
};
use tidebreak_harness::{ApprovalDecision, HarnessApprovalRef, HarnessEvent};

/// A chat event the engine could not translate on its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Lookup {
    /// `ask_user_questions` parked the turn; the questions live in the store.
    Questions { call_id: CallId },
    /// `exit_plan_mode` parked the turn; the plan body lives in the store.
    Plan { call_id: CallId },
}

/// One chat event, translated.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Translated {
    /// Emit these, in order. Empty when the event has no adapter analogue.
    Emit(Vec<HarnessEvent>),
    /// Finish the translation with a store read.
    Lookup(Lookup),
}

/// Cut `text` to at most `max` characters on a character boundary.
pub(super) fn bounded(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut cut = text.chars().take(max.saturating_sub(1)).collect::<String>();
    cut.push('…');
    cut
}

/// Token accounting in the adapter's shape.
pub(super) fn code_usage(usage: Usage) -> CodeUsage {
    CodeUsage {
        input_tokens: u64::from(usage.input_tokens),
        output_tokens: u64::from(usage.output_tokens),
        cache_read_input_tokens: u64::from(usage.cache_read_input_tokens),
        cache_creation_input_tokens: u64::from(usage.cache_creation_input_tokens),
        context_tokens: 0,
        first_call_context_tokens: None,
    }
}

/// The display classification for a described action.
pub(super) fn tool_detail(preview: &ToolActionPreview) -> ToolDetail {
    match preview {
        ToolActionPreview::Exec {
            command, args, cwd, ..
        } => ToolDetail::Command {
            cmd: bounded(
                &std::iter::once(command.as_str())
                    .chain(args.iter().map(String::as_str))
                    .collect::<Vec<_>>()
                    .join(" "),
                MAX_TOOL_SUMMARY_CHARS,
            ),
            cwd: cwd.clone(),
        },
        ToolActionPreview::WriteFile { path, .. } => ToolDetail::FileEdit {
            path: bounded(path, MAX_TOOL_SUMMARY_CHARS),
        },
        ToolActionPreview::Search { query, .. } | ToolActionPreview::WebSearch { query, .. } => {
            ToolDetail::Search {
                query: bounded(query, MAX_TOOL_SUMMARY_CHARS),
            }
        }
        other => ToolDetail::Other {
            summary: bounded(
                other.summary().unwrap_or("tool call"),
                MAX_TOOL_SUMMARY_CHARS,
            ),
        },
    }
}

/// A completed tool call's outcome, from its output.
pub(super) fn tool_outcome(output: &ToolOutput) -> ToolOutcome {
    match (output.is_error, output.error_category) {
        (false, _) => ToolOutcome::Succeeded,
        (true, Some(ToolErrorCategory::UserDeclined | ToolErrorCategory::UserCancelled)) => {
            ToolOutcome::Denied
        }
        (true, _) => ToolOutcome::Failed,
    }
}

/// A tool approval as the adapter states it: the exact server-built preview
/// and the grant ladder the engine offers, or a plain summary when the call
/// could not be described (no ladder is offered without a description).
pub(super) fn tool_use_kind(
    tool_name: &str,
    preview: Option<ToolActionPreview>,
    grant_scopes: Vec<GrantScope>,
) -> CodeApprovalKind {
    match preview {
        Some(preview) => CodeApprovalKind::ToolUse {
            preview,
            offered_grants: grant_scopes,
        },
        None => CodeApprovalKind::Other {
            summary: bounded(tool_name, MAX_TOOL_SUMMARY_CHARS),
        },
    }
}

/// The decision the engine observed on its own channel, in adapter terms.
pub(super) fn observed_decision(approved: bool) -> ApprovalDecision {
    if approved {
        ApprovalDecision::Approve
    } else {
        ApprovalDecision::Deny { feedback: None }
    }
}

/// The chat-side decision for an adapter decision on a tool approval.
///
/// Answers and plan decisions never reach here: they settle a parked
/// continuation through its own store operation.
pub(super) fn chat_decision(decision: &ApprovalDecision) -> Option<ChatDecision> {
    match decision {
        ApprovalDecision::Approve | ApprovalDecision::ApproveWithGrant { .. } => {
            Some(ChatDecision::Approve)
        }
        ApprovalDecision::Deny { feedback } => Some(ChatDecision::Reject {
            reason: feedback
                .as_deref()
                .filter(|feedback| !feedback.trim().is_empty())
                .map_or_else(
                    || tidebreak_core::ToolApproval::DEFAULT_REJECT_REASON.to_owned(),
                    |feedback| bounded(feedback, tidebreak_core::ToolApproval::MAX_REASON_BYTES),
                ),
        }),
        ApprovalDecision::Answers { .. } | ApprovalDecision::PlanDecision { .. } => None,
    }
}

pub(super) fn failure(error: &AgentErrorInfo) -> HarnessEvent {
    HarnessEvent::TurnFailed {
        error: BoundedError {
            message: bounded(
                &format!("{}: {}", error.kind, error.message),
                MAX_NOTICE_CHARS,
            ),
        },
    }
}

pub(super) fn refusal_notice(refusal: &RefusalOutcome) -> HarnessEvent {
    let category = refusal
        .category()
        .map_or(String::new(), |category| format!(" ({category})"));
    HarnessEvent::HarnessNotice {
        level: HarnessNoticeLevel::Warning,
        message: bounded(
            &format!("The model declined to continue{category}."),
            MAX_NOTICE_CHARS,
        ),
    }
}

pub(super) fn notice(message: &str) -> HarnessEvent {
    HarnessEvent::HarnessNotice {
        level: HarnessNoticeLevel::Info,
        message: bounded(message, MAX_NOTICE_CHARS),
    }
}

pub(super) fn assistant_message(text: &str) -> HarnessEvent {
    HarnessEvent::AssistantMessage {
        text: bounded(text, MAX_EVENT_TEXT_CHARS),
        parent_call_id: None,
    }
}

pub(super) fn tool_started(call_id: &CallId, name: &str) -> HarnessEvent {
    HarnessEvent::ToolStarted {
        call_id: call_id.to_string(),
        name: name.to_owned(),
        detail: ToolDetail::Other {
            summary: bounded(name, MAX_TOOL_SUMMARY_CHARS),
        },
        parent_call_id: None,
    }
}

pub(super) fn tool_completed(
    call_id: &CallId,
    output: &ToolOutput,
    action: Option<&ToolActionPreview>,
) -> HarnessEvent {
    HarnessEvent::ToolCompleted {
        call_id: call_id.to_string(),
        outcome: tool_outcome(output),
        preview: bounded(&output.content, MAX_PREVIEW_CHARS),
        detail: action.map(tool_detail),
        parent_call_id: None,
    }
}

pub(super) fn approval_requested(
    call_id: &CallId,
    raw: serde_json::Value,
    kind: CodeApprovalKind,
) -> HarnessEvent {
    HarnessEvent::ApprovalRequested {
        harness_ref: HarnessApprovalRef::engine(call_id.to_string()),
        raw,
        kind: Some(kind),
    }
}

pub(super) fn approval_resolved(call_id: &CallId, decision: ApprovalDecision) -> HarnessEvent {
    HarnessEvent::ApprovalResolved {
        harness_ref: HarnessApprovalRef::engine(call_id.to_string()),
        decision,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_cuts_on_a_character_boundary() {
        assert_eq!(bounded("héllo", 10), "héllo");
        assert_eq!(bounded("héllo wörld", 6), "héllo…");
    }

    #[test]
    fn a_declined_tool_reads_as_denied() {
        let declined = ToolOutput::failed(ToolErrorCategory::UserDeclined, "no");
        assert_eq!(tool_outcome(&declined), ToolOutcome::Denied);
        let broken = ToolOutput::failed(ToolErrorCategory::TransportFailed, "boom");
        assert_eq!(tool_outcome(&broken), ToolOutcome::Failed);
        assert_eq!(
            tool_outcome(&ToolOutput::text("ok")),
            ToolOutcome::Succeeded
        );
    }

    #[test]
    fn an_undescribed_call_offers_no_grant_ladder() {
        let kind = tool_use_kind("mystery", None, vec![GrantScope::WholeTool]);
        assert_eq!(
            kind,
            CodeApprovalKind::Other {
                summary: "mystery".into()
            }
        );
    }

    #[test]
    fn a_deny_without_feedback_carries_the_default_reason() {
        let ChatDecision::Reject { reason } =
            chat_decision(&ApprovalDecision::Deny { feedback: None }).unwrap()
        else {
            panic!("deny maps to reject");
        };
        assert_eq!(reason, tidebreak_core::ToolApproval::DEFAULT_REJECT_REASON);
        assert!(chat_decision(&ApprovalDecision::PlanDecision {
            approve: true,
            feedback: None
        })
        .is_none());
    }
}
