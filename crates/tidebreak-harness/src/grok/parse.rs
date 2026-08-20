//! Parse captured Grok CLI `streaming-json` lines into [`HarnessEvent`]s.
//!
//! Written only against the checked-in fixtures under `fixtures/grok/1.0.4/`.
//! Unknown event types increment a counter and are logged (size-capped). They
//! are never fatal and never dropped silently.

use std::collections::HashSet;

use serde_json::Value;
use tidebreak_core::{
    BoundedError, CodeUsage, HarnessKind, HarnessNoticeLevel, ToolDetail, ToolOutcome,
    MAX_EVENT_TEXT_CHARS, MAX_NOTICE_CHARS, MAX_PREVIEW_CHARS,
};

use crate::HarnessEvent;

/// Longest unrecognized payload kept for the debug log.
const MAX_UNRECOGNIZED_LOG: usize = 512;

/// Incremental parser for one Grok CLI print-mode `streaming-json` stream.
#[derive(Debug)]
pub struct GrokStreamParser {
    unrecognized: u64,
    resume_ref: Option<String>,
    version: String,
    started_tools: HashSet<String>,
    completed_tools: HashSet<String>,
    emitted_session: bool,
    last_usage: CodeUsage,
    pending_text: String,
}

impl Default for GrokStreamParser {
    fn default() -> Self {
        Self {
            unrecognized: 0,
            resume_ref: None,
            version: "unknown".into(),
            started_tools: HashSet::new(),
            completed_tools: HashSet::new(),
            emitted_session: false,
            last_usage: CodeUsage::default(),
            pending_text: String::new(),
        }
    }
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

impl GrokStreamParser {
    /// Empty parser.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the observed CLI version for [`HarnessEvent::SessionStarted`].
    /// The 1.0.4 stream does not carry a version field.
    pub fn set_version(&mut self, version: impl Into<String>) {
        self.version = version.into();
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
        parser.set_version("1.0.4");
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
            "text" => self.parse_text(value),
            "thought" => self.parse_thought(value),
            "tool_call" => self.parse_tool_call(value),
            "tool_call_update" => self.parse_tool_call_update(value),
            "usage" => self.parse_usage(value),
            "end" => self.parse_end(value),
            "error" => self.parse_error(value),
            "available_commands" => {
                // The command inventory is stable, known startup metadata and
                // does not belong in the normalized transcript.
                Vec::new()
            }
            "plan" | "max_turns_reached" => {
                self.count_unrecognized(kind, value);
                Vec::new()
            }
            other => {
                self.count_unrecognized(other, value);
                Vec::new()
            }
        }
    }

    fn parse_text(&mut self, value: &Value) -> Vec<HarnessEvent> {
        let text = value.get("data").and_then(Value::as_str).unwrap_or("");
        if text.is_empty() {
            return Vec::new();
        }
        self.pending_text.push_str(text);
        vec![HarnessEvent::AssistantDelta {
            text: bound(text, MAX_EVENT_TEXT_CHARS),
        }]
    }

    fn parse_thought(&mut self, value: &Value) -> Vec<HarnessEvent> {
        let text = value.get("data").and_then(Value::as_str).unwrap_or("");
        if text.is_empty() {
            return Vec::new();
        }
        vec![HarnessEvent::ReasoningDelta {
            text: bound(text, MAX_EVENT_TEXT_CHARS),
        }]
    }

    fn parse_tool_call(&mut self, value: &Value) -> Vec<HarnessEvent> {
        let call_id = value
            .get("toolCallId")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        if call_id.is_empty() || !self.started_tools.insert(call_id.clone()) {
            return Vec::new();
        }
        let name = value
            .get("toolName")
            .or_else(|| value.get("title"))
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned();
        let input = value.get("rawInput").cloned().unwrap_or(Value::Null);
        vec![HarnessEvent::ToolStarted {
            call_id,
            name: name.clone(),
            detail: tool_detail(&name, value.get("kind").and_then(Value::as_str), &input),
            parent_call_id: None,
        }]
    }

    fn parse_tool_call_update(&mut self, value: &Value) -> Vec<HarnessEvent> {
        // `status: null` is the documented progress frame — the manifest
        // records the observed set as `null (progress) | completed | failed`.
        // It carries no state we normalize, so dropping it is a deliberate
        // no-op rather than an unrecognized event.
        let Some(status) = value.get("status").and_then(Value::as_str) else {
            return Vec::new();
        };
        let call_id = value
            .get("toolCallId")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        if call_id.is_empty() || !self.completed_tools.insert(call_id.clone()) {
            return Vec::new();
        }
        let preview = tool_preview(value);
        let outcome = match status {
            "completed" => ToolOutcome::Succeeded,
            "failed" if preview.to_ascii_lowercase().contains("denied") => ToolOutcome::Denied,
            "failed" | "cancelled" => ToolOutcome::Failed,
            other => {
                self.completed_tools.remove(&call_id);
                self.count_unrecognized(&format!("tool_call_update/{other}"), value);
                return Vec::new();
            }
        };
        vec![HarnessEvent::ToolCompleted {
            call_id,
            outcome,
            preview,
            // `tool_call` already carries the whole `rawInput`, so the started
            // call names its subject. `tool_call_update` repeats no arguments
            // at all, so there is nothing to correct it with.
            detail: None,
            parent_call_id: None,
        }]
    }

    fn parse_usage(&mut self, value: &Value) -> Vec<HarnessEvent> {
        self.last_usage = usage_from(value.get("usage"));
        self.flush_assistant()
    }

    fn parse_end(&mut self, value: &Value) -> Vec<HarnessEvent> {
        if let Some(session_id) = value.get("sessionId").and_then(Value::as_str) {
            self.resume_ref = Some(session_id.to_owned());
        }
        if let Some(usage) = value.get("usage") {
            self.last_usage = usage_from(Some(usage));
        }
        let mut events = self.emit_session();
        events.extend(self.flush_assistant());
        let stop = value
            .get("stopReason")
            .and_then(Value::as_str)
            .unwrap_or("");
        match stop {
            "cancelled" => events.push(HarnessEvent::TurnInterrupted),
            "end_turn" => events.push(HarnessEvent::TurnCompleted {
                usage: self.last_usage.clone(),
            }),
            // A stop reason this build has never seen still ends the turn —
            // the child has exited and something must close it out. Folding it
            // into a plain completion without a word would be exactly the
            // silent normalization decision 0031 forbids, so it is counted and
            // stated before the turn closes.
            other => {
                let label = if other.is_empty() { "missing" } else { other };
                self.count_unrecognized(&format!("end/stopReason/{label}"), value);
                events.push(HarnessEvent::HarnessNotice {
                    level: HarnessNoticeLevel::Warning,
                    message: bound(
                        &format!(
                            "the engine ended the turn with an unrecognized stop reason \
                             ({label}); it was recorded as completed"
                        ),
                        MAX_NOTICE_CHARS,
                    ),
                });
                events.push(HarnessEvent::TurnCompleted {
                    usage: self.last_usage.clone(),
                });
            }
        }
        events
    }

    fn parse_error(&mut self, value: &Value) -> Vec<HarnessEvent> {
        let message = value
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("engine reported an error");
        let mut events = self.flush_assistant();
        events.push(HarnessEvent::TurnFailed {
            error: BoundedError {
                message: bound(message, MAX_NOTICE_CHARS),
            },
        });
        events
    }

    fn emit_session(&mut self) -> Vec<HarnessEvent> {
        if self.emitted_session {
            return Vec::new();
        }
        self.emitted_session = true;
        vec![HarnessEvent::SessionStarted {
            harness_kind: HarnessKind::Grok,
            harness_version: self.version.clone(),
            resume_ref: self.resume_ref.clone(),
        }]
    }

    fn flush_assistant(&mut self) -> Vec<HarnessEvent> {
        if self.pending_text.is_empty() {
            return Vec::new();
        }
        let text = std::mem::take(&mut self.pending_text);
        vec![HarnessEvent::AssistantMessage {
            text: bound(&text, MAX_EVENT_TEXT_CHARS),
            parent_call_id: None,
        }]
    }

    fn count_unrecognized(&mut self, label: &str, payload: impl std::fmt::Display) {
        self.unrecognized += 1;
        let mut rendered = payload.to_string();
        crate::text::truncate_on_char_boundary(&mut rendered, MAX_UNRECOGNIZED_LOG);
        tracing::debug!(
            target: "tidebreak_harness::grok",
            unrecognized = self.unrecognized,
            kind = label,
            payload = %rendered,
            "unrecognized engine event"
        );
    }
}

fn tool_detail(name: &str, kind: Option<&str>, input: &Value) -> ToolDetail {
    let path = input
        .get("target_file")
        .or_else(|| input.get("file_path"))
        .or_else(|| input.get("path"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    match (name, kind) {
        ("read_file", _) | (_, Some("read")) => ToolDetail::FileRead { path },
        ("write" | "search_replace", _) | (_, Some("write" | "edit")) => {
            ToolDetail::FileEdit { path }
        }
        ("run_terminal_command", _) | (_, Some("execute")) => ToolDetail::Command {
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
        ("grep" | "list_dir" | "web_search", _) | (_, Some("search")) => ToolDetail::Search {
            query: input
                .get("pattern")
                .or_else(|| input.get("query"))
                .or_else(|| input.get("target_directory"))
                .and_then(Value::as_str)
                .unwrap_or(name)
                .to_owned(),
        },
        _ => ToolDetail::Other {
            summary: name.to_owned(),
        },
    }
}

fn tool_preview(value: &Value) -> String {
    if let Some(text) = content_text(value.get("content")) {
        if !text.is_empty() {
            return bound(&text, MAX_PREVIEW_CHARS);
        }
    }
    if let Some(output) = value.get("rawOutput") {
        let rendered = match output {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        return bound(&rendered, MAX_PREVIEW_CHARS);
    }
    String::new()
}

fn content_text(value: Option<&Value>) -> Option<String> {
    let items = value?.as_array()?;
    let mut parts = Vec::new();
    for item in items {
        if let Some(text) = item
            .pointer("/content/text")
            .or_else(|| item.get("text"))
            .and_then(Value::as_str)
        {
            parts.push(text.to_owned());
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
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
{"type":"text","data":"hi"}
{"type":"brand_new_shape","foo":1}
{"type":"end","stopReason":"end_turn","sessionId":"abc","usage":{"input_tokens":1,"output_tokens":2}}
"#;
        let out = GrokStreamParser::parse_ndjson(input);
        assert!(out.unrecognized >= 1);
        assert!(out
            .events
            .iter()
            .any(|event| matches!(event, HarnessEvent::AssistantDelta { text } if text == "hi")));
        assert!(matches!(
            out.events.last(),
            Some(HarnessEvent::TurnCompleted { .. })
        ));
        assert_eq!(out.resume_ref.as_deref(), Some("abc"));
    }

    #[test]
    fn an_unknown_stop_reason_is_counted_and_stated_while_a_progress_frame_is_not() {
        // `status: null` on tool_call_update is the manifest's documented
        // progress frame: a deliberate no-op, not a drop. An unheard-of stop
        // reason is the opposite — it still ends the turn, but it is counted.
        let input = r#"
{"type":"tool_call","toolCallId":"call-1","toolName":"read_file","rawInput":{}}
{"type":"tool_call_update","toolCallId":"call-1","status":null}
{"type":"end","stopReason":"budget_exhausted","sessionId":"abc"}
"#;
        let out = GrokStreamParser::parse_ndjson(input);
        assert_eq!(out.unrecognized, 1);
        assert!(out.events.iter().any(|event| matches!(
            event,
            HarnessEvent::HarnessNotice {
                level: HarnessNoticeLevel::Warning,
                ..
            }
        )));
        assert!(matches!(
            out.events.last(),
            Some(HarnessEvent::TurnCompleted { .. })
        ));
    }
}
