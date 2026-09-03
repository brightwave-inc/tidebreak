//! Parse captured Claude Code `stream-json` lines into [`HarnessEvent`]s.
//!
//! Written only against the checked-in fixtures under
//! `fixtures/claude-code/2.1.233/`. Unknown event types increment a counter
//! and are logged (size-capped). They are never fatal and never dropped
//! silently.

use std::collections::HashMap;

use serde_json::Value;
use tidebreak_core::{
    BoundedError, CodeUsage, HarnessKind, HarnessNoticeLevel, ToolDetail, ToolOutcome,
    MAX_EVENT_TEXT_CHARS, MAX_NOTICE_CHARS, MAX_PREVIEW_CHARS,
};

use crate::HarnessEvent;

/// Longest unrecognized payload kept for the debug log.
const MAX_UNRECOGNIZED_LOG: usize = 512;

/// Longest assembled tool-argument JSON held while a call streams in.
///
/// A `Write` streams a whole file through this channel, and none of it is
/// needed to name the call, so the buffer stops rather than grows.
const MAX_TOOL_INPUT_JSON: usize = 16 * 1024;

/// Incremental parser for one Claude Code print-mode stream.
#[derive(Debug, Default)]
pub struct ClaudeStreamParser {
    unrecognized: u64,
    resume_ref: Option<String>,
    version: Option<String>,
    /// call id → best detail emitted or recorded for that call so far.
    started_tools: HashMap<String, ToolDetail>,
    /// call id → detail to correct the started call with, once it resolves.
    late_details: HashMap<String, ToolDetail>,
    /// open block → a call opened with no arguments yet, held until they
    /// assemble.
    open_blocks: HashMap<BlockKey, OpenToolCall>,
    emitted_session: bool,
}

/// Which content block a stream event belongs to.
///
/// Two subagents stream at once (decision 52) and each numbers its blocks
/// from zero, so the index alone does not identify one. The `Task` call the
/// lines run inside separates them.
type BlockKey = (Option<String>, u64);

/// A tool call the engine has opened but not yet described.
#[derive(Debug)]
struct OpenToolCall {
    call_id: String,
    name: String,
    parent_call_id: Option<String>,
    /// `input_json_delta` fragments, joined in arrival order.
    input_json: String,
    /// The fragments outgrew [`MAX_TOOL_INPUT_JSON`] and were dropped.
    overflowed: bool,
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
            "hook_started"
            | "hook_response"
            | "status"
            | "thinking_tokens"
            | "session_state_changed" => {
                // Known lifecycle/telemetry frames that carry no transcript
                // state. They are recognized no-ops, not protocol drift.
                // `session_state_changed` brackets every turn on a
                // session-long child (observed on 2.1.238).
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
                    Some("input_json_delta") => {
                        if let (Some(key), Some(fragment)) = (
                            block_key(value, event),
                            delta.get("partial_json").and_then(Value::as_str),
                        ) {
                            self.accumulate_tool_input(&key, fragment);
                        }
                        Vec::new()
                    }
                    Some("signature_delta") => Vec::new(),
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
                    let parent = parent_call_id(value);
                    return self.open_tool_call(&block, parent, block_key(value, event));
                }
                Vec::new()
            }
            "content_block_stop" => match block_key(value, event) {
                Some(key) => self.close_tool_block(&key),
                None => Vec::new(),
            },
            "message_start" | "message_delta" | "message_stop" | "ping" => Vec::new(),
            other => {
                self.count_unrecognized(&format!("stream_event/{other}"), event);
                Vec::new()
            }
        }
    }

    fn parse_assistant(&mut self, value: &Value) -> Vec<HarnessEvent> {
        let mut events = Vec::new();
        // Every line a subagent produces carries the parent `Task` call's id
        // at the top level (decision 52); the parent's own lines say null.
        let parent = parent_call_id(value);
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
                            parent_call_id: parent.clone(),
                        });
                    }
                }
                Some("tool_use") => {
                    events.extend(self.emit_tool_started(&block, parent.clone()));
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
        let parent = parent_call_id(value);
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
                        let detail = self.late_details.remove(&call_id);
                        // A call whose arguments never assembled is still
                        // held. Its start has to reach the transcript before
                        // its completion does.
                        events.extend(self.flush_open_call(&call_id));
                        events.push(HarnessEvent::ToolCompleted {
                            call_id,
                            outcome: if is_error {
                                ToolOutcome::Failed
                            } else {
                                ToolOutcome::Succeeded
                            },
                            preview,
                            detail,
                            parent_call_id: parent.clone(),
                        });
                    }
                }
                Some("text") => {
                    let text = block.get("text").and_then(Value::as_str).unwrap_or("");
                    // Claude presents a subagent's prompt as a nested user
                    // message. It belongs to the parent Agent call; only a
                    // top-level user message is steering from the person.
                    if parent.is_none() && !text.is_empty() {
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

    /// Take one view of a tool-use block from an `assistant` message.
    fn emit_tool_started(&mut self, block: &Value, parent: Option<String>) -> Vec<HarnessEvent> {
        let call_id = tool_call_id(block);
        if call_id.is_empty() {
            return Vec::new();
        }
        let name = tool_name(block);
        let input = block.get("input").cloned().unwrap_or(Value::Null);
        let detail = tool_detail(&name, &input);
        self.record_tool_view(call_id, &name, parent, detail)
    }

    /// Hold a call the engine just opened until its arguments assemble.
    ///
    /// `content_block_start` opens the call with `input: {}` and the
    /// arguments stream in after it as `input_json_delta`, so the opening
    /// view names nothing. Starting the call there leaves a supervising UI
    /// showing a nameless card for as long as the tool runs — a `Bash` call
    /// that times out after 60s spends all 60s unlabelled. The call waits
    /// instead for the first view that carries its arguments: whichever of
    /// `content_block_stop` and the `assistant` message repeating the block
    /// arrives first. Both land before the engine runs the tool.
    fn open_tool_call(
        &mut self,
        block: &Value,
        parent: Option<String>,
        key: Option<BlockKey>,
    ) -> Vec<HarnessEvent> {
        let call_id = tool_call_id(block);
        if call_id.is_empty() {
            return Vec::new();
        }
        let name = tool_name(block);
        let input = block.get("input").cloned().unwrap_or(Value::Null);
        let holdable =
            key.filter(|_| arguments_pending(&input) && !self.started_tools.contains_key(&call_id));
        let Some(key) = holdable else {
            let detail = tool_detail(&name, &input);
            return self.record_tool_view(call_id, &name, parent, detail);
        };
        self.open_blocks.insert(
            key,
            OpenToolCall {
                call_id,
                name,
                parent_call_id: parent,
                input_json: String::new(),
                overflowed: false,
            },
        );
        Vec::new()
    }

    /// Buffer one `input_json_delta` fragment for the call held at `key`.
    fn accumulate_tool_input(&mut self, key: &BlockKey, fragment: &str) {
        let Some(open) = self.open_blocks.get_mut(key) else {
            return;
        };
        if open.overflowed {
            return;
        }
        if open.input_json.len() + fragment.len() > MAX_TOOL_INPUT_JSON {
            open.overflowed = true;
            open.input_json = String::new();
            return;
        }
        open.input_json.push_str(fragment);
    }

    /// Start the call held at `key`, named by the arguments it streamed.
    fn close_tool_block(&mut self, key: &BlockKey) -> Vec<HarnessEvent> {
        let Some(open) = self.open_blocks.remove(key) else {
            return Vec::new();
        };
        // Arguments that overran the buffer, or that never parsed, leave the
        // call as unnamed as it was when it opened. The correction on
        // `ToolCompleted` still applies.
        let input = if open.overflowed {
            Value::Null
        } else {
            serde_json::from_str::<Value>(&open.input_json).unwrap_or(Value::Null)
        };
        let detail = tool_detail(&open.name, &input);
        self.record_tool_view(open.call_id, &open.name, open.parent_call_id, detail)
    }

    /// Start a held call early, when something downstream needs it started.
    fn flush_open_call(&mut self, call_id: &str) -> Vec<HarnessEvent> {
        let held = self
            .open_blocks
            .iter()
            .find_map(|(key, open)| (open.call_id == call_id).then(|| key.clone()));
        match held {
            Some(key) => self.close_tool_block(&key),
            None => Vec::new(),
        }
    }

    /// Fold one view of a call into the stream.
    ///
    /// The first view starts the call. A later, more specific one is a
    /// correction and rides the call's `ToolCompleted`. A `Task` detail
    /// stays `Other` at every view (equal specificity), so it upgrades on
    /// any change: the assembled description is the subagent's display name
    /// (decision 52).
    fn record_tool_view(
        &mut self,
        call_id: String,
        name: &str,
        parent: Option<String>,
        detail: ToolDetail,
    ) -> Vec<HarnessEvent> {
        if let Some(started) = self.started_tools.get(&call_id) {
            let corrected = detail.specificity() > started.specificity()
                || (name == "Task"
                    && detail != *started
                    && detail.specificity() >= started.specificity());
            if corrected {
                self.started_tools.insert(call_id.clone(), detail.clone());
                self.late_details.insert(call_id, detail);
            }
            return Vec::new();
        }
        self.open_blocks.retain(|_, open| open.call_id != call_id);
        self.started_tools.insert(call_id.clone(), detail.clone());
        vec![HarnessEvent::ToolStarted {
            call_id,
            name: name.to_owned(),
            detail,
            parent_call_id: parent,
        }]
    }

    fn count_unrecognized(&mut self, label: &str, payload: impl std::fmt::Display) {
        self.unrecognized += 1;
        let mut rendered = payload.to_string();
        crate::text::truncate_on_char_boundary(&mut rendered, MAX_UNRECOGNIZED_LOG);
        // The kind alone is what makes a dropped event findable later, and
        // it carries no engine payload, so it rides at info. The truncated
        // body stays at debug for whoever is actually chasing one.
        tracing::info!(
            target: "tidebreak_harness::claude",
            unrecognized = self.unrecognized,
            kind = label,
            "unrecognized engine event"
        );
        tracing::debug!(
            target: "tidebreak_harness::claude",
            kind = label,
            payload = %rendered,
            "unrecognized engine event payload"
        );
    }
}

/// The block a stream event belongs to: its index, under the `Task` call the
/// line runs inside.
fn block_key(value: &Value, event: &Value) -> Option<BlockKey> {
    let index = event.get("index").and_then(Value::as_u64)?;
    Some((parent_call_id(value), index))
}

/// Engine-native id of a `tool_use` block.
fn tool_call_id(block: &Value) -> String {
    block
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}

/// Tool name of a `tool_use` block.
fn tool_name(block: &Value) -> String {
    block
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_owned()
}

/// Whether a view of a call still says nothing about its arguments.
fn arguments_pending(input: &Value) -> bool {
    input.as_object().is_none_or(serde_json::Map::is_empty)
}

/// The top-level `parent_tool_use_id` a subagent's lines carry. The parent's
/// own lines say null, which reads as `None`.
fn parent_call_id(value: &Value) -> Option<String> {
    value
        .get("parent_tool_use_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
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
        // A `Task` call spans a subagent (decision 52). Its description is
        // the name a rail row shows, so surface it over the bare tool name.
        "Task" => {
            let description = input
                .get("description")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty());
            let subagent_type = input
                .get("subagent_type")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty());
            let summary = match (description, subagent_type) {
                (Some(description), Some(kind)) => format!("{description} ({kind})"),
                (Some(description), None) => description.to_owned(),
                (None, Some(kind)) => kind.to_owned(),
                (None, None) => name.to_owned(),
            };
            ToolDetail::Other { summary }
        }
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
        context_tokens: context_tokens_from(value),
        first_call_context_tokens: first_call_context_tokens_from(value),
    }
}

/// The prompt resident on the turn's last model call.
///
/// `result.usage` carries `iterations`, one entry per API call, beside the
/// turn totals it already sums into the four spend counts. The last entry is
/// the call that closed the turn, so its three prompt-side counts are what the
/// window actually held. Without this, a ten-call turn reports ten prompts.
///
/// A `result` with no `iterations` came from a single call, so the top-level
/// object is that call and the same three-way sum is correct for it.
fn context_tokens_from(usage: &Value) -> u64 {
    let call = usage
        .get("iterations")
        .and_then(Value::as_array)
        .and_then(|iterations| iterations.last())
        .unwrap_or(usage);
    let field = |name: &str| call.get(name).and_then(Value::as_u64).unwrap_or(0);
    field("input_tokens")
        .saturating_add(field("cache_read_input_tokens"))
        .saturating_add(field("cache_creation_input_tokens"))
}

fn first_call_context_tokens_from(usage: &Value) -> Option<u64> {
    let call = usage
        .get("iterations")
        .and_then(Value::as_array)
        .and_then(|iterations| iterations.first())?;
    let tokens = call
        .get("input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .saturating_add(
            call.get("cache_read_input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        )
        .saturating_add(
            call.get("cache_creation_input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        );
    (tokens > 0).then_some(tokens)
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

    /// `usage.iterations` is one entry per API call. Summing the turn totals
    /// counts the transcript once per call, so a three-call turn reads as
    /// three prompts; the window only ever held the last one.
    #[test]
    fn context_tokens_come_from_the_last_iteration() {
        let usage = serde_json::json!({
            "input_tokens": 6,
            "output_tokens": 196,
            "cache_read_input_tokens": 142_916,
            "cache_creation_input_tokens": 5_479,
            "iterations": [
                {"input_tokens": 2, "output_tokens": 80,
                 "cache_read_input_tokens": 44_122, "cache_creation_input_tokens": 5_211},
                {"input_tokens": 2, "output_tokens": 91,
                 "cache_read_input_tokens": 49_334, "cache_creation_input_tokens": 127},
                {"input_tokens": 2, "output_tokens": 25,
                 "cache_read_input_tokens": 49_460, "cache_creation_input_tokens": 141}
            ]
        });
        let parsed = usage_from(Some(&usage));

        assert_eq!(parsed.context_tokens, 49_603);
        assert_eq!(parsed.first_call_context_tokens, Some(49_335));
        // The four spend counts keep the turn totals they already carried.
        assert_eq!(parsed.cache_read_input_tokens, 142_916);
        assert_eq!(parsed.output_tokens, 196);
    }

    /// A single-call turn reports no `iterations`, and the object itself is
    /// that call.
    #[test]
    fn context_tokens_fall_back_to_the_result_itself() {
        let usage = serde_json::json!({
            "input_tokens": 12,
            "output_tokens": 4,
            "cache_read_input_tokens": 8_192,
            "cache_creation_input_tokens": 100
        });
        assert_eq!(usage_from(Some(&usage)).context_tokens, 8_304);

        // An empty array is the same situation as a missing one.
        let empty = serde_json::json!({"input_tokens": 12, "iterations": []});
        assert_eq!(usage_from(Some(&empty)).context_tokens, 12);
    }

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

    #[test]
    fn stream_ping_is_a_known_noop() {
        let out =
            ClaudeStreamParser::parse_ndjson(r#"{"type":"stream_event","event":{"type":"ping"}}"#);
        assert_eq!(out.unrecognized, 0);
        assert!(out.events.is_empty());
    }

    /// The captured streams repeat a finished block on an `assistant` line
    /// just before its `content_block_stop`, so that view is what names a
    /// call in practice. The arguments are the engine's own guarantee
    /// though: they stream in as `input_json_delta` and are complete at the
    /// stop. Without the `assistant` line the call still starts named.
    #[test]
    fn a_call_starts_named_from_the_streamed_arguments_alone() {
        let input = r#"
{"type":"stream_event","event":{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"Bash","input":{}}}}
{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"comm"}}}
{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"and\": \"ls -R\"}"}}}
{"type":"stream_event","event":{"type":"content_block_stop","index":1}}
"#;
        let out = ClaudeStreamParser::parse_ndjson(input);
        assert_eq!(
            out.events,
            vec![HarnessEvent::ToolStarted {
                call_id: "toolu_1".into(),
                name: "Bash".into(),
                detail: ToolDetail::Command {
                    cmd: "ls -R".into(),
                    cwd: String::new(),
                },
                parent_call_id: None,
            }]
        );
    }

    /// Holding a call until its arguments assemble must not lose it. A
    /// result for a call still in flight starts it first, so no completion
    /// ever lands on a call the transcript never opened.
    #[test]
    fn a_result_starts_a_call_whose_arguments_never_assembled() {
        let input = r#"
{"type":"stream_event","event":{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"Bash","input":{}}}}
{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"done"}]}}
"#;
        let out = ClaudeStreamParser::parse_ndjson(input);
        assert!(matches!(
            out.events.first(),
            Some(HarnessEvent::ToolStarted { call_id, .. }) if call_id == "toolu_1"
        ));
        assert!(matches!(
            out.events.last(),
            Some(HarnessEvent::ToolCompleted { call_id, .. }) if call_id == "toolu_1"
        ));
    }
}
