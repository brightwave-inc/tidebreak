//! The chat journal as an alias over the code journal.
//!
//! Since decision 0048 step 5 there is one journal: `event`, with
//! [`Event`] as its vocabulary. The chat surface keeps reading
//! [`AgentEvent`] rows because that is the contract its clients bind to, so
//! the chat lane's events are written as `Event` rows through
//! [`journal_row`] and read back through [`chat_event`]. The two are a
//! round trip: every field a chat event carries has a home on the code row,
//! and the chat journal fixture (`fixtures/journal-events.json`) is the
//! tripwire that keeps it so.
//!
//! This is the aliasing the decision's amendment permits — one route family
//! over one set of entities — and nothing else maps one side's entities into
//! the other's shapes. Rows only an external engine writes (`SessionStarted`,
//! `FileChanged`, `AttentionChanged`, …) have no chat reading and project to
//! nothing.

use crate::code::{
    ApprovalDecisionKind, ApprovalId, BoundedError, Event, InternalApprovalRequest, ToolDetail,
    ToolOutcome, TurnUsage, MAX_NOTICE_CHARS, MAX_PREVIEW_CHARS, MAX_TOOL_SUMMARY_CHARS,
};
use crate::error::{AgentError, Result};
use crate::event::AgentEvent;
use crate::id::{CallId, MessageId, TurnId};
use crate::preview::ToolActionPreview;
use crate::provider::Usage;
use crate::tool::{ToolErrorCategory, ToolOutput};

/// The code journal row for one chat event.
///
/// Total: every chat event has a row. Fields the code vocabulary derives from
/// the chat payload — a tool's outcome and preview, a failure's bounded
/// message — are filled in beside the structured original, so a code reader
/// sees what an external engine would have reported and the chat reader gets
/// its event back exactly.
#[must_use]
pub fn journal_row(event: &AgentEvent) -> Event {
    match event {
        AgentEvent::TurnStarted { turn_id } => Event::TurnStarted {
            turn_id: TurnId(turn_id.0),
        },
        AgentEvent::TextDelta { text } => Event::AssistantDelta { text: text.clone() },
        AgentEvent::ReasoningDelta { text } => Event::ReasoningDelta { text: text.clone() },
        AgentEvent::StreamInterrupted => Event::StreamInterrupted,
        AgentEvent::ToolCallStarted { call_id, name } => Event::ToolStarted {
            call_id: call_id.to_string(),
            name: name.clone(),
            detail: ToolDetail::Other {
                summary: bounded(name, MAX_TOOL_SUMMARY_CHARS),
            },
            parent_call_id: None,
        },
        AgentEvent::ToolCallArgsDelta { call_id, fragment } => Event::ToolArgsDelta {
            call_id: call_id.to_string(),
            fragment: fragment.clone(),
        },
        AgentEvent::UserQuestionsAsked { call_id, turn_id } => Event::ApprovalRequested {
            approval_id: approval_id_of(*call_id),
            request: Some(InternalApprovalRequest::Questions {
                turn_id: TurnId(turn_id.0),
            }),
        },
        AgentEvent::ApprovalRequired {
            auto_judging,
            call_id,
            tool_name,
            class,
            kind,
            grant_scopes,
            preview,
        } => Event::ApprovalRequested {
            approval_id: approval_id_of(*call_id),
            request: Some(InternalApprovalRequest::ToolUse {
                auto_judging: *auto_judging,
                tool_name: tool_name.clone(),
                class: *class,
                approval: *kind,
                grant_scopes: grant_scopes.clone(),
                preview: preview.clone(),
            }),
        },
        AgentEvent::ApprovalDecided { call_id, approved } => Event::ApprovalResolved {
            approval_id: approval_id_of(*call_id),
            decision: if *approved {
                ApprovalDecisionKind::Approve
            } else {
                ApprovalDecisionKind::Deny { feedback: None }
            },
            actor: None,
        },
        AgentEvent::ToolCallCompleted {
            call_id,
            output,
            action,
            result,
        } => Event::ToolCompleted {
            call_id: call_id.to_string(),
            outcome: tool_outcome(output),
            preview: bounded(&output.content, MAX_PREVIEW_CHARS),
            output: Some(Box::new(output.clone())),
            action: action.clone(),
            result: result.clone(),
            detail: action.as_ref().map(tool_detail),
            parent_call_id: None,
        },
        AgentEvent::TurnCompleted { usage, stop_reason } => Event::TurnCompleted {
            usage: code_usage(*usage),
            checkpoint: None,
            stop_reason: Some(*stop_reason),
        },
        AgentEvent::TurnRefused { usage, refusal } => Event::TurnRefused {
            usage: code_usage(*usage),
            refusal: refusal.clone(),
        },
        AgentEvent::TurnFailed { error } => Event::TurnFailed {
            error: BoundedError {
                message: bounded(
                    &format!("{}: {}", error.kind, error.message),
                    MAX_NOTICE_CHARS,
                ),
            },
            detail: Some(error.clone()),
        },
        AgentEvent::TurnCancelled { usage } => Event::TurnInterrupted {
            usage: Some(code_usage(*usage)),
        },
        AgentEvent::UserSteered {
            message_id,
            content,
        } => Event::UserSteered {
            text: content.clone(),
            message_id: Some(message_id.0),
        },
        AgentEvent::ContextTruncated {
            original_tokens,
            fitted_tokens,
        } => Event::ContextTruncated {
            original_tokens: *original_tokens,
            fitted_tokens: *fitted_tokens,
        },
        AgentEvent::CompactionStarted => Event::CompactionStarted,
        AgentEvent::CompactionFinished { compacted } => Event::CompactionFinished {
            compacted: *compacted,
        },
        AgentEvent::PlanProposed { call_id, turn_id } => Event::ApprovalRequested {
            approval_id: approval_id_of(*call_id),
            request: Some(InternalApprovalRequest::Plan {
                turn_id: TurnId(turn_id.0),
            }),
        },
        AgentEvent::TaskPlanUpdated { call_id, turn_id } => Event::TaskPlanUpdated {
            call_id: call_id.to_string(),
            turn_id: TurnId(turn_id.0),
        },
    }
}

/// The chat event a code journal row replays as, or `None` for a row only
/// an external engine writes.
///
/// Fails only on a row that claims the chat shape and cannot be read — a
/// call id that is not a UUID — which is corruption, not a code-only row.
pub fn chat_event(event: Event) -> Result<Option<AgentEvent>> {
    Ok(Some(match event {
        Event::TurnStarted { turn_id } => AgentEvent::TurnStarted {
            turn_id: TurnId(turn_id.0),
        },
        Event::AssistantDelta { text } => AgentEvent::TextDelta { text },
        Event::ReasoningDelta { text } => AgentEvent::ReasoningDelta { text },
        Event::StreamInterrupted => AgentEvent::StreamInterrupted,
        Event::ToolStarted { call_id, name, .. } => AgentEvent::ToolCallStarted {
            call_id: call_id_of(&call_id)?,
            name,
        },
        Event::ToolArgsDelta { call_id, fragment } => AgentEvent::ToolCallArgsDelta {
            call_id: call_id_of(&call_id)?,
            fragment,
        },
        Event::ApprovalRequested {
            approval_id,
            request: Some(request),
        } => match request {
            InternalApprovalRequest::ToolUse {
                auto_judging,
                tool_name,
                class,
                approval,
                grant_scopes,
                preview,
            } => AgentEvent::ApprovalRequired {
                auto_judging,
                call_id: CallId(approval_id.0),
                tool_name,
                class,
                kind: approval,
                grant_scopes,
                preview,
            },
            InternalApprovalRequest::Questions { turn_id } => AgentEvent::UserQuestionsAsked {
                call_id: CallId(approval_id.0),
                turn_id: TurnId(turn_id.0),
            },
            InternalApprovalRequest::Plan { turn_id } => AgentEvent::PlanProposed {
                call_id: CallId(approval_id.0),
                turn_id: TurnId(turn_id.0),
            },
        },
        // A consent card's decision. The structured resolutions — answers,
        // a plan verdict — settle a parked continuation whose chat fact is
        // the call's own completion row, journaled with the answer.
        Event::ApprovalResolved {
            approval_id,
            decision:
                decision @ (ApprovalDecisionKind::Approve
                | ApprovalDecisionKind::ApprovedWithGrant { .. }
                | ApprovalDecisionKind::Deny { .. }
                | ApprovalDecisionKind::Abandoned),
            ..
        } => AgentEvent::ApprovalDecided {
            call_id: CallId(approval_id.0),
            approved: matches!(
                decision,
                ApprovalDecisionKind::Approve | ApprovalDecisionKind::ApprovedWithGrant { .. }
            ),
        },
        Event::ToolCompleted {
            call_id,
            output: Some(output),
            action,
            result,
            ..
        } => AgentEvent::ToolCallCompleted {
            call_id: call_id_of(&call_id)?,
            output: *output,
            action,
            result,
        },
        Event::TurnCompleted {
            usage,
            stop_reason: Some(stop_reason),
            ..
        } => AgentEvent::TurnCompleted {
            usage: chat_usage(&usage),
            stop_reason,
        },
        Event::TurnRefused { usage, refusal } => AgentEvent::TurnRefused {
            usage: chat_usage(&usage),
            refusal,
        },
        Event::TurnFailed {
            detail: Some(error),
            ..
        } => AgentEvent::TurnFailed { error },
        Event::TurnInterrupted { usage: Some(usage) } => AgentEvent::TurnCancelled {
            usage: chat_usage(&usage),
        },
        Event::UserSteered {
            text,
            message_id: Some(message_id),
        } => AgentEvent::UserSteered {
            message_id: MessageId(message_id),
            content: text,
        },
        Event::ContextTruncated {
            original_tokens,
            fitted_tokens,
        } => AgentEvent::ContextTruncated {
            original_tokens,
            fitted_tokens,
        },
        Event::CompactionStarted => AgentEvent::CompactionStarted,
        Event::CompactionFinished { compacted } => AgentEvent::CompactionFinished { compacted },
        Event::TaskPlanUpdated { call_id, turn_id } => AgentEvent::TaskPlanUpdated {
            call_id: call_id_of(&call_id)?,
            turn_id: TurnId(turn_id.0),
        },
        // Rows only an external engine writes, and code-shaped rows the
        // chat lane never produces (a completion with no stop reason, a
        // steer with no message row, a failure with no kind).
        Event::SessionStarted { .. }
        | Event::TurnResumed { .. }
        | Event::AssistantMessage { .. }
        | Event::ToolCompleted { output: None, .. }
        | Event::FileChanged { .. }
        | Event::ApprovalRequested { request: None, .. }
        | Event::ApprovalResolved {
            decision:
                ApprovalDecisionKind::Answered { .. } | ApprovalDecisionKind::PlanDecided { .. },
            ..
        }
        | Event::TurnCompleted {
            stop_reason: None, ..
        }
        | Event::TurnFailed { detail: None, .. }
        | Event::TurnInterrupted { usage: None }
        | Event::UserSteered {
            message_id: None, ..
        }
        | Event::CheckpointRecorded { .. }
        | Event::HarnessNotice { .. }
        | Event::AttentionChanged { .. } => return Ok(None),
    }))
}

/// Decode a stored journal payload as the chat event it replays as.
pub fn decode_chat_event(payload: serde_json::Value) -> Result<Option<AgentEvent>> {
    chat_event(serde_json::from_value::<Event>(payload)?)
}

/// Decode a stored journal payload that must be a chat event.
///
/// The recovery receipts — a terminal event, an approval's journal row, a
/// checkpoint's completion — are written by the chat lane and read back to
/// compare against what the lane meant to write; a code-only row there is
/// a broken receipt.
pub fn decode_chat_event_required(payload: serde_json::Value) -> Result<AgentEvent> {
    decode_chat_event(payload)?
        .ok_or_else(|| AgentError::Store("journal receipt is not a chat event".into()))
}

/// The approval row an internal-engine card is parked on: the row's id is
/// the call id, so the chat surface recovers one from the other.
#[must_use]
pub fn approval_id_of(call_id: CallId) -> ApprovalId {
    ApprovalId(call_id.0)
}

fn call_id_of(raw: &str) -> Result<CallId> {
    raw.parse::<CallId>().map_err(|_| {
        AgentError::Store(format!(
            "journal row names a call id that is not a UUID: {raw}"
        ))
    })
}

/// Cut `text` to at most `max` characters on a character boundary.
#[must_use]
pub fn bounded(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut cut = text.chars().take(max.saturating_sub(1)).collect::<String>();
    cut.push('…');
    cut
}

/// Token accounting in the code journal's shape.
#[must_use]
pub fn code_usage(usage: Usage) -> TurnUsage {
    TurnUsage {
        input_tokens: u64::from(usage.input_tokens),
        output_tokens: u64::from(usage.output_tokens),
        cache_read_input_tokens: u64::from(usage.cache_read_input_tokens),
        cache_creation_input_tokens: u64::from(usage.cache_creation_input_tokens),
        context_tokens: 0,
        first_call_context_tokens: None,
    }
}

/// Token accounting in the chat's shape. Lossless for what the chat lane
/// wrote; a count only an external engine could report saturates.
#[must_use]
pub fn chat_usage(usage: &TurnUsage) -> Usage {
    Usage {
        input_tokens: u32::try_from(usage.input_tokens).unwrap_or(u32::MAX),
        output_tokens: u32::try_from(usage.output_tokens).unwrap_or(u32::MAX),
        cache_read_input_tokens: u32::try_from(usage.cache_read_input_tokens).unwrap_or(u32::MAX),
        cache_creation_input_tokens: u32::try_from(usage.cache_creation_input_tokens)
            .unwrap_or(u32::MAX),
    }
}

/// The display classification for a described action.
#[must_use]
pub fn tool_detail(preview: &ToolActionPreview) -> ToolDetail {
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
#[must_use]
pub fn tool_outcome(output: &ToolOutput) -> ToolOutcome {
    match (output.is_error, output.error_category) {
        (false, _) => ToolOutcome::Succeeded,
        (true, Some(ToolErrorCategory::UserDeclined | ToolErrorCategory::UserCancelled)) => {
            ToolOutcome::Denied
        }
        (true, _) => ToolOutcome::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::HarnessKind;

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

    /// A row only an external engine writes has no chat reading, and a
    /// code-shaped terminal row the chat lane never produced is not
    /// invented into one.
    #[test]
    fn external_rows_project_to_nothing() {
        for event in [
            Event::SessionStarted {
                harness_kind: HarnessKind::ClaudeCode,
                harness_version: "1".into(),
                resume_ref: None,
            },
            Event::AssistantMessage {
                text: "hi".into(),
                parent_call_id: None,
            },
            Event::TurnCompleted {
                usage: TurnUsage::default(),
                checkpoint: None,
                stop_reason: None,
            },
            Event::TurnInterrupted { usage: None },
            Event::TurnResumed {
                turn_id: TurnId::new(),
            },
            Event::UserSteered {
                text: "go".into(),
                message_id: None,
            },
        ] {
            assert_eq!(chat_event(event).unwrap(), None);
        }
    }

    /// The internal engine's consent card and its parks are one approval
    /// row each; the chat surface reads the card back from the row id and
    /// the request the row journaled beside it. An external adapter's row
    /// carries no request and replays as nothing.
    #[test]
    fn an_internal_approval_row_replays_as_the_chat_card() {
        let call_id = CallId::new();
        let turn_id = TurnId::new();
        let asked = AgentEvent::UserQuestionsAsked { call_id, turn_id };
        let row = journal_row(&asked);
        assert_eq!(
            row,
            Event::ApprovalRequested {
                approval_id: approval_id_of(call_id),
                request: Some(InternalApprovalRequest::Questions {
                    turn_id: TurnId(turn_id.0),
                }),
            }
        );
        assert_eq!(chat_event(row).unwrap(), Some(asked));
        let decided = AgentEvent::ApprovalDecided {
            call_id,
            approved: false,
        };
        assert_eq!(chat_event(journal_row(&decided)).unwrap(), Some(decided));
        // A structured resolution is the park's own; the chat fact is the
        // completion the answer journals.
        assert_eq!(
            chat_event(Event::ApprovalResolved {
                actor: None,
                approval_id: approval_id_of(call_id),
                decision: ApprovalDecisionKind::Answered {
                    answers: Vec::new()
                },
            })
            .unwrap(),
            None
        );
        assert_eq!(
            chat_event(Event::ApprovalRequested {
                approval_id: approval_id_of(call_id),
                request: None,
            })
            .unwrap(),
            None
        );
    }

    #[test]
    fn a_chat_shaped_row_with_a_bad_call_id_is_corruption_not_silence() {
        let row = Event::ToolArgsDelta {
            call_id: "toolu_1".into(),
            fragment: "{".into(),
        };
        assert!(chat_event(row).is_err());
    }
}
