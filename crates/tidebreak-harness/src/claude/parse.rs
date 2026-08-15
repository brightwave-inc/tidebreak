//! Parse captured Claude Code `stream-json` lines into [`HarnessEvent`]s.
//!
//! Written only against the checked-in fixtures under
//! `fixtures/claude-code/2.1.233/`. Unknown event types increment a counter
//! and are logged (size-capped). They are never fatal and never dropped
//! silently.

use std::collections::HashSet;

use serde_json::Value;
use tidebreak_core::{
    BoundedError, CodeUsage, HarnessKind, HarnessNoticeLevel, ToolDetail, ToolOutcome,
    MAX_EVENT_TEXT_CHARS, MAX_NOTICE_CHARS, MAX_PREVIEW_CHARS,
};

use crate::HarnessEvent;

/// Longest unrecognized payload kept for the debug log.
const MAX_UNRECOGNIZED_LOG: usize = 512;

/// Incremental parser for one Claude Code print-mode stream.
#[derive(Debug, Default)]
pub struct ClaudeStreamParser {
    unrecognized: u64,
    resume_ref: Option<String>,
    version: Option<String>,
    started_tools: HashSet<String>,
    emitted_session: bool,
}

/// Result of parsing a whole fixture or a finished stream.
#[derive(Debug, Clone, PartialEq)]
pub struct ParseOutcome {
    /// Normalized events, in order.
    pub events: Vec<HarnessEvent>,
    /// Count of unknown or unmapped event types.
    pub unrecognized: u64,
    /// Session id extracted from the stream, when any.
    pub resume_ref: Option<String>,
}

impl ClaudeStreamParser {
    /// Empty parser.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Unrecognized-event count so far.
    #[must_use]
    pub fn unrecognized(&self) -> u64 {
        self.unrecognized
    }

    /// Resume ref extracted so far.
    #[must_use]
    pub fn resume_ref(&self) -> Option<&str> {
        self.resume_ref.as_deref()
    }

    /// Parse one NDJSON line. Never returns an error: unknown shapes increment
    /// [`Self::unrecognized`].
    pub fn push_line(&mut self, line: &str) -> Vec<HarnessEvent> {
        let line = line.trim();
        if line.is_empty() {
            return Vec::new();
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            self.count_unrecognized("unparseable-line", line);
            return Vec::new();
        };
        self.push_value(&value)
    }

    /// Parse a whole captured NDJSON document.
    pub fn parse_ndjson(input: &str) -> ParseOutcome {
        let mut parser = Self::new();
        let mut events = Vec::new();
        for line in input.lines() {
            events.extend(parser.push_line(line));
        }
        ParseOutcome {
            events,
            unrecognized: parser.unrecognized,
            resume_ref: parser.resume_ref,
        }
    }

    fn push_value(&mut self, value: &Value) -> Vec<HarnessEvent> {
        let Some(kind) = value.get("type").and_then(Value::as_str) else {
            self.count_unrecognized("missing-type", value);
            return Vec::new();
        };
        match kind {
            "system" => self.parse_system(value),
            "stream_event" => self.parse_stream_event(value),
            "assistant" => self.parse_assistant(value),
            "user" => self.parse_user(value),
            "result" => self.parse_result(value),
            "control_response" => Vec::new(),
            other => {
                self.count_unrecognized(other, value);
                Vec::new()
            }
        }
    }

    fn parse_system(&mut self, value: &Value) -> Vec<HarnessEvent> {
        let subtype = value.get("subtype").and_then(Value::as_str).unwrap_or("");
        match subtype {
            "init" => {
                if let Some(session_id) = value.get("session_id").and_then(Value::as_str) {
                    self.resume_ref = Some(session_id.to_owned());
                }
                if let Some(version) = value.get("claude_code_version").and_then(Value::as_str) {
                    self.version = Some(version.to_owned());
                }
                if self.emitted_session {
                    return Vec::new();
                }
                self.emitted_session = true;
                vec![HarnessEvent::SessionStarted {
                    harness_kind: HarnessKind::ClaudeCode,
                    harness_version: self.version.clone().unwrap_or_else(|| "unknown".into()),
                    resume_ref: self.resume_ref.clone(),
                }]
            }
            "permission_denied" => {
                let message = value
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("permission denied")
                    .to_owned();
                // `permission_denied` is an already-resolved denial, not a
                // parked request. A live permission-prompt tool is what
                // produces real ApprovalRequested / ApprovalResolved events;
                // synthesizing either would mint or miss approval rows.
                // The following tool_result already conveys the denial.
                vec![HarnessEvent::HarnessNotice {
                    level: HarnessNoticeLevel::Warning,
                    message: bound(&message, MAX_NOTICE_CHARS),
                }]
            }
            "hook_started" | "hook_response" | "status" | "thinking_tokens" => {
                // Known stream noise. Counted so it is never silent.
                self.count_unrecognized(&format!("system/{subtype}"), value);
                Vec::new()
            }
            other => {
                self.count_unrecognized(&format!("system/{other}"), value);
                Vec::new()
            }
        }
    }

    fn parse_stream_event(&mut self, value: &Value) -> Vec<HarnessEvent> {
        let Some(event) = value.get("event") else {
            self.count_unrecognized("stream_event/missing", value);
            return Vec::new();
        };
        let Some(inner) = event.get("type").and_then(Value::as_str) else {
            self.count_unrecognized("stream_event/untyped", value);
            return Vec::new();
        };
        match inner {
            "content_block_delta" => {
                let delta = event.get("delta").cloned().unwrap_or(Value::Null);
                match delta.get("type").and_then(Value::as_str) {
                    Some("text_delta") => {
                        let text = delta.get("text").and_then(Value::as_str).unwrap_or("");
                        if text.is_empty() {
                            Vec::new()
                        } else {
                            vec![HarnessEvent::AssistantDelta {
                                text: bound(text, MAX_EVENT_TEXT_CHARS),
                            }]
                        }
                    }
                    Some("thinking_delta") => {
                        let text = delta.get("thinking").and_then(Value::as_str).unwrap_or("");
                        if text.is_empty() {
                            Vec::new()
                        } else {
                            vec![HarnessEvent::ReasoningDelta {
                                text: bound(text, MAX_EVENT_TEXT_CHARS),
                            }]
                        }
                    }
                    Some("input_json_delta") | Some("signature_delta") => Vec::new(),
                    Some(other) => {
                        self.count_unrecognized(&format!("stream_event/delta/{other}"), &delta);
                        Vec::new()
                    }
                    None => Vec::new(),
                }
            }
            "content_block_start" => {
                let block = event.get("content_block").cloned().unwrap_or(Value::Null);
                if block.get("type").and_then(Value::as_str) == Some("tool_use") {
                    return self.emit_tool_started(&block);
                }
                Vec::new()
            }
            "message_start" | "message_delta" | "message_stop" | "content_block_stop" => Vec::new(),
            other => {
                self.count_unrecognized(&format!("stream_event/{other}"), event);
                Vec::new()
            }
        }
    }

    fn parse_assistant(&mut self, value: &Value) -> Vec<HarnessEvent> {
        let mut events = Vec::new();
        let content = value
            .pointer("/message/content")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for block in content {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    let text = block.get("text").and_then(Value::as_str).unwrap_or("");
                    if !text.is_empty() {
                        events.push(HarnessEvent::AssistantMessage {
                            text: bound(text, MAX_EVENT_TEXT_CHARS),
                        });
                    }
                }
                Some("tool_use") => {
                    events.extend(self.emit_tool_started(&block));
                }
                Some("thinking") => {}
                Some(other) => {
                    self.count_unrecognized(&format!("assistant/{other}"), &block);
                }
                None => {}
            }
        }
        events
    }

    fn parse_user(&mut self, value: &Value) -> Vec<HarnessEvent> {
        let mut events = Vec::new();
        let content = match value.get("message") {
            Some(Value::Object(map)) => map.get("content").cloned().unwrap_or(Value::Null),
            Some(other) => other.clone(),
            None => Value::Null,
        };
        let blocks: Vec<Value> = match content {
            Value::Array(items) => items,
            Value::Object(_) => vec![content],
            _ => Vec::new(),
        };
        for block in blocks {
            match block.get("type").and_then(Value::as_str) {
                Some("tool_result") => {
                    let call_id = block
                        .get("tool_use_id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned();
                    let is_error = block.get("is_error").and_then(Value::as_bool) == Some(true);
                    let preview = tool_result_preview(&block);
                    if !call_id.is_empty() {
                        events.push(HarnessEvent::ToolCompleted {
                            call_id,
                            outcome: if is_error {
                                ToolOutcome::Failed
                            } else {
                                ToolOutcome::Succeeded
                            },
                            preview,
                        });
                    }
                }
                Some("text") => {
                    let text = block.get("text").and_then(Value::as_str).unwrap_or("");
                    if !text.is_empty() {
                        events.push(HarnessEvent::UserSteered {
                            text: bound(text, MAX_EVENT_TEXT_CHARS),
                        });
                    }
                }
                Some(other) => {
                    self.count_unrecognized(&format!("user/{other}"), &block);
                }
                None => {}
            }
        }
        events
    }

    fn parse_result(&mut self, value: &Value) -> Vec<HarnessEvent> {
        if let Some(session_id) = value.get("session_id").and_then(Value::as_str) {
            if self.resume_ref.is_none() {
                self.resume_ref = Some(session_id.to_owned());
            }
        }
        let is_error = value.get("is_error").and_then(Value::as_bool) == Some(true);
        // Only the captured interrupt fixture (`terminal_reason:
        // aborted_streaming`) is an interruption. Any other error —
        // including a missing terminal_reason — is a failure.
        let interrupted =
            value.get("terminal_reason").and_then(Value::as_str) == Some("aborted_streaming");
        if interrupted {
            return vec![HarnessEvent::TurnInterrupted];
        }
        if is_error {
            let message = value
                .get("result")
                .and_then(Value::as_str)
                .unwrap_or("engine reported an error");
            return vec![HarnessEvent::TurnFailed {
                error: BoundedError {
                    message: bound(message, MAX_NOTICE_CHARS),
                },
            }];
        }
        vec![HarnessEvent::TurnCompleted {
            usage: usage_from(value.get("usage")),
        }]
    }

    fn emit_tool_started(&mut self, block: &Value) -> Vec<HarnessEvent> {
        let call_id = block
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        if call_id.is_empty() || !self.started_tools.insert(call_id.clone()) {
            return Vec::new();
        }
        let name = block
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned();
        let input = block.get("input").cloned().unwrap_or(Value::Null);
        vec![HarnessEvent::ToolStarted {
            call_id,
            name: name.clone(),
            detail: tool_detail(&name, &input),
        }]
    }

    fn count_unrecognized(&mut self, label: &str, payload: impl std::fmt::Display) {
        self.unrecognized += 1;
        let mut rendered = payload.to_string();
        if rendered.len() > MAX_UNRECOGNIZED_LOG {
            rendered.truncate(MAX_UNRECOGNIZED_LOG);
        }
        tracing::debug!(
            target: "tidebreak_harness::claude",
            unrecognized = self.unrecognized,
            kind = label,
            payload = %rendered,
            "unrecognized engine event"
        );
    }
}

fn tool_detail(name: &str, input: &Value) -> ToolDetail {
    let path = input
        .get("file_path")
        .or_else(|| input.get("path"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    match name {
        "Read" | "NotebookRead" => ToolDetail::FileRead { path },
        "Write" | "Edit" | "NotebookEdit" => ToolDetail::FileEdit { path },
        "Bash" => ToolDetail::Command {
            cmd: input
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            cwd: input
                .get("cwd")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
        },
        "Grep" | "Glob" | "WebSearch" => ToolDetail::Search {
            query: input
                .get("pattern")
                .or_else(|| input.get("query"))
                .and_then(Value::as_str)
                .unwrap_or(name)
                .to_owned(),
        },
        _ => ToolDetail::Other {
            summary: name.to_owned(),
        },
    }
}

fn tool_result_preview(block: &Value) -> String {
    let content = block.get("content");
    let text = match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    };
    bound(&text, MAX_PREVIEW_CHARS)
}

fn usage_from(value: Option<&Value>) -> CodeUsage {
    let Some(value) = value else {
        return CodeUsage::default();
    };
    CodeUsage {
        input_tokens: value
            .get("input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        output_tokens: value
            .get("output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cache_read_input_tokens: value
            .get("cache_read_input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cache_creation_input_tokens: value
            .get("cache_creation_input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    }
}

fn bound(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_owned()
    } else {
        text.chars().take(max).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_event_types_are_counted_and_do_not_drop_known_events() {
        let input = r#"
{"type":"system","subtype":"init","session_id":"abc","claude_code_version":"2.1.233"}
{"type":"brand_new_shape","foo":1}
{"type":"result","subtype":"success","is_error":false,"terminal_reason":"completed","usage":{"input_tokens":1,"output_tokens":2}}
"#;
        let out = ClaudeStreamParser::parse_ndjson(input);
        assert!(out.unrecognized >= 1);
        assert!(matches!(
            out.events.first(),
            Some(HarnessEvent::SessionStarted { .. })
        ));
        assert!(matches!(
            out.events.last(),
            Some(HarnessEvent::TurnCompleted { .. })
        ));
        assert_eq!(out.resume_ref.as_deref(), Some("abc"));
    }
}
