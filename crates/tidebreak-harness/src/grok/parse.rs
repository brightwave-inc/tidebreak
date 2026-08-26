//! Parse captured Grok CLI `streaming-json` lines into [`HarnessEvent`]s.
//!
//! Written only against the checked-in fixtures under `fixtures/grok/1.0.4/`
//! and the supplemental subagent projection under `fixtures/grok/1.0.5/`.
//! Unknown event types increment a counter and are logged (size-capped). They
//! are never fatal and never dropped silently.

use std::collections::{HashMap, HashSet};

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
    pending_spawns: HashMap<String, PendingSpawn>,
    companion_calls: HashMap<String, Vec<CompanionTarget>>,
    settled_subagents: HashSet<String>,
    emitted_session: bool,
    last_usage: CodeUsage,
    /// Prompt tokens on the most recent `usage` event, which is one model
    /// call. Kept apart from `last_usage` because the closing `end` event
    /// overwrites that with the turn's cumulative total.
    last_call_context_tokens: Option<u64>,
    /// Prompt tokens on the first per-call usage event in this turn.
    first_call_context_tokens: Option<u64>,
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
            pending_spawns: HashMap::new(),
            companion_calls: HashMap::new(),
            settled_subagents: HashSet::new(),
            emitted_session: false,
            last_usage: CodeUsage::default(),
            last_call_context_tokens: None,
            first_call_context_tokens: None,
            pending_text: String::new(),
        }
    }
}

#[derive(Debug, Clone)]
struct PendingSpawn {
    detail: ToolDetail,
    background: bool,
}

#[derive(Debug, Clone)]
struct CompanionTarget {
    task_id: String,
    parent_call_id: String,
    call_id: String,
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
        // Grok's background spawn call resolves immediately with a durable
        // `subagent_id`; that id, not this four-millisecond launcher call, is
        // the spanning identity later output/kill tools address. Delay the
        // normalized Task start until the completion frame reveals it.
        if name == "spawn_subagent" {
            self.pending_spawns.insert(
                call_id,
                PendingSpawn {
                    detail: subagent_detail(&input),
                    background: input
                        .get("background")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                },
            );
            return Vec::new();
        }

        let targets = companion_targets(&call_id, &input);
        if !targets.is_empty() && is_subagent_companion(&name) {
            self.companion_calls.insert(call_id, targets.clone());
            return targets
                .into_iter()
                .map(|target| HarnessEvent::ToolStarted {
                    call_id: target.call_id,
                    name: name.clone(),
                    detail: companion_detail(&name, &target.task_id),
                    parent_call_id: Some(target.parent_call_id),
                })
                .collect();
        }
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
            if let Some(call_id) = value.get("toolCallId").and_then(Value::as_str) {
                if let Some(spawn) = self.pending_spawns.get_mut(call_id) {
                    if let Some(input) = value.get("rawInput") {
                        spawn.detail = subagent_detail(input);
                        spawn.background |= input
                            .get("run_in_background")
                            .or_else(|| input.get("background"))
                            .and_then(Value::as_bool)
                            .unwrap_or(false);
                    }
                }
            }
            return Vec::new();
        };
        let call_id = value
            .get("toolCallId")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        if matches!(status, "in_progress" | "inProgress" | "running") {
            if let Some(spawn) = self.pending_spawns.get_mut(&call_id) {
                if let Some(input) = value.get("rawInput") {
                    spawn.detail = subagent_detail(input);
                }
            }
            return Vec::new();
        }
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

        if let Some(spawn) = self.pending_spawns.remove(&call_id) {
            return self.complete_spawn(&call_id, spawn, outcome, preview);
        }

        if let Some(targets) = self.companion_calls.remove(&call_id) {
            return self.complete_companion(value, targets, outcome, &preview);
        }

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

    fn complete_spawn(
        &mut self,
        launcher_call_id: &str,
        spawn: PendingSpawn,
        outcome: ToolOutcome,
        preview: String,
    ) -> Vec<HarnessEvent> {
        let subagent_id = subagent_id_from(preview.as_str());
        let task_call_id = subagent_id
            .clone()
            .unwrap_or_else(|| launcher_call_id.to_owned());
        let mut events = vec![HarnessEvent::ToolStarted {
            call_id: task_call_id.clone(),
            name: "Task".into(),
            detail: spawn.detail,
            parent_call_id: None,
        }];

        let started_in_background = spawn.background
            || preview
                .to_ascii_lowercase()
                .contains("subagent started in background");
        if outcome == ToolOutcome::Succeeded && subagent_id.is_some() && started_in_background {
            return events;
        }

        if outcome == ToolOutcome::Succeeded && !preview.trim().is_empty() {
            events.push(HarnessEvent::AssistantMessage {
                text: bound(&preview, MAX_EVENT_TEXT_CHARS),
                parent_call_id: Some(task_call_id.clone()),
            });
        }
        self.settled_subagents.insert(task_call_id.clone());
        events.push(HarnessEvent::ToolCompleted {
            call_id: task_call_id,
            outcome,
            preview,
            detail: None,
            parent_call_id: None,
        });
        events
    }

    fn complete_companion(
        &mut self,
        value: &Value,
        targets: Vec<CompanionTarget>,
        outcome: ToolOutcome,
        fallback_preview: &str,
    ) -> Vec<HarnessEvent> {
        let mut events = Vec::new();
        let allow_unkeyed_result = targets.len() == 1;
        for target in targets {
            let result = task_result(value, &target.task_id, allow_unkeyed_result);
            let preview = result
                .and_then(task_result_preview)
                .unwrap_or_else(|| fallback_preview.to_owned());
            events.push(HarnessEvent::ToolCompleted {
                call_id: target.call_id,
                outcome,
                preview: bound(&preview, MAX_PREVIEW_CHARS),
                detail: None,
                parent_call_id: Some(target.parent_call_id.clone()),
            });

            let Some(task_outcome) = result.and_then(task_result_outcome) else {
                continue;
            };
            if !self.settled_subagents.insert(target.parent_call_id.clone()) {
                continue;
            }
            if !preview.trim().is_empty() {
                events.push(HarnessEvent::AssistantMessage {
                    text: bound(&preview, MAX_EVENT_TEXT_CHARS),
                    parent_call_id: Some(target.parent_call_id.clone()),
                });
            }
            events.push(HarnessEvent::ToolCompleted {
                call_id: target.parent_call_id,
                outcome: task_outcome,
                preview: bound(&preview, MAX_PREVIEW_CHARS),
                detail: None,
                parent_call_id: None,
            });
        }
        events
    }

    fn parse_usage(&mut self, value: &Value) -> Vec<HarnessEvent> {
        let usage = usage_from(value.get("usage"));
        // A `usage` event is one model call: `tool-use.ndjson` reports 9050
        // then 139 fresh input across two calls, and the closing `end` event
        // reports their 9189 sum. So this call's prompt-side sum is an
        // occupancy reading; the `end` event's is spend.
        self.last_call_context_tokens = Some(usage.context_tokens);
        if self.first_call_context_tokens.is_none() && usage.context_tokens > 0 {
            self.first_call_context_tokens = Some(usage.context_tokens);
        }
        self.last_usage = usage;
        self.flush_assistant()
    }

    fn parse_end(&mut self, value: &Value) -> Vec<HarnessEvent> {
        if let Some(session_id) = value.get("sessionId").and_then(Value::as_str) {
            self.resume_ref = Some(session_id.to_owned());
        }
        if let Some(usage) = value.get("usage") {
            self.last_usage = usage_from(Some(usage));
        }
        // The `end` usage sums every model call, so its own prompt-side sum
        // is roughly one prompt per call. The last per-call `usage` event is
        // the prompt that was still resident when the turn ended. A turn that
        // published no per-call event keeps the fallback, which is correct
        // for the single-call case the two shapes agree on.
        if let Some(context_tokens) = self.last_call_context_tokens {
            self.last_usage.context_tokens = context_tokens;
        }
        self.last_usage.first_call_context_tokens = self.first_call_context_tokens;
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
            // A stop reason this build has never seen is protocol drift. The
            // child has exited, so close the turn as a visible failure.
            other => {
                let label = if other.is_empty() { "missing" } else { other };
                self.count_unrecognized(&format!("end/stopReason/{label}"), value);
                events.push(HarnessEvent::HarnessNotice {
                    level: HarnessNoticeLevel::Warning,
                    message: bound(
                        &format!(
                            "the engine ended the turn with an unrecognized stop reason \
                             ({label}); it was recorded as failed"
                        ),
                        MAX_NOTICE_CHARS,
                    ),
                });
                events.push(HarnessEvent::TurnFailed {
                    error: BoundedError {
                        message: bound(
                            &format!("Grok protocol drift: turn ended with stop reason {label}"),
                            MAX_NOTICE_CHARS,
                        ),
                    },
                });
            }
        }
        self.last_call_context_tokens = None;
        self.first_call_context_tokens = None;
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
        // The kind alone is what makes a dropped event findable later, and
        // it carries no engine payload, so it rides at info. The truncated
        // body stays at debug for whoever is actually chasing one.
        tracing::info!(
            target: "tidebreak_harness::grok",
            unrecognized = self.unrecognized,
            kind = label,
            "unrecognized engine event"
        );
        tracing::debug!(
            target: "tidebreak_harness::grok",
            kind = label,
            payload = %rendered,
            "unrecognized engine event payload"
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

fn subagent_detail(input: &Value) -> ToolDetail {
    let description = input
        .get("description")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            input
                .get("prompt")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
        })
        .unwrap_or("Subagent");
    let kind = input
        .get("subagent_type")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty());
    ToolDetail::Other {
        summary: match kind {
            Some(kind) => format!("{description} ({kind})"),
            None => description.to_owned(),
        },
    }
}

fn is_subagent_companion(name: &str) -> bool {
    matches!(
        name,
        "get_command_or_subagent_output"
            | "wait_commands_or_subagents"
            | "kill_command_or_subagent"
    )
}

fn companion_targets(call_id: &str, input: &Value) -> Vec<CompanionTarget> {
    let mut task_ids = Vec::new();
    if let Some(task_id) = input.get("task_id").and_then(Value::as_str) {
        task_ids.push(task_id);
    }
    if let Some(values) = input.get("task_ids").and_then(Value::as_array) {
        task_ids.extend(values.iter().filter_map(Value::as_str));
    }
    task_ids
        .into_iter()
        .filter(|task_id| looks_like_subagent_id(task_id))
        .map(|task_id| CompanionTarget {
            task_id: task_id.to_owned(),
            parent_call_id: task_id.to_owned(),
            call_id: format!("{call_id}:{task_id}"),
        })
        .collect()
}

fn looks_like_subagent_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && [8, 13, 18, 23]
            .into_iter()
            .all(|index| bytes.get(index) == Some(&b'-'))
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| [8, 13, 18, 23].contains(&index) || byte.is_ascii_hexdigit())
}

fn companion_detail(name: &str, task_id: &str) -> ToolDetail {
    let action = match name {
        "kill_command_or_subagent" => "Cancel subagent",
        "wait_commands_or_subagents" => "Wait for subagent",
        _ => "Check subagent output",
    };
    ToolDetail::Other {
        summary: format!("{action} {}", &task_id[..8.min(task_id.len())]),
    }
}

fn subagent_id_from(preview: &str) -> Option<String> {
    preview.lines().find_map(|line| {
        line.trim()
            .strip_prefix("subagent_id:")
            .map(str::trim)
            .filter(|value| looks_like_subagent_id(value))
            .map(str::to_owned)
    })
}

fn task_result<'a>(
    value: &'a Value,
    task_id: &str,
    allow_unkeyed_result: bool,
) -> Option<&'a Value> {
    if let Some(results) = value
        .pointer("/rawOutput/MultiResult/results")
        .and_then(Value::as_array)
    {
        return results
            .iter()
            .find(|result| result.get("task_id").and_then(Value::as_str) == Some(task_id));
    }
    let result = value.pointer("/rawOutput/Result")?;
    match result.get("task_id").and_then(Value::as_str) {
        Some(result_task_id) if result_task_id == task_id => Some(result),
        None if allow_unkeyed_result => Some(result),
        _ => None,
    }
}

fn task_result_preview(result: &Value) -> Option<String> {
    result
        .get("output")
        .or_else(|| result.get("message"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn task_result_outcome(result: &Value) -> Option<ToolOutcome> {
    match result
        .get("status")
        .or_else(|| result.get("outcome"))
        .and_then(Value::as_str)
    {
        Some("completed" | "success" | "succeeded") => Some(ToolOutcome::Succeeded),
        Some("failed" | "not_found" | "cancelled" | "canceled" | "killed") => {
            Some(ToolOutcome::Failed)
        }
        Some("running" | "pending") | None => None,
        Some(_) => None,
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
    let field = |name: &str| value.get(name).and_then(Value::as_u64).unwrap_or(0);
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
        // Correct for a per-call `usage` event, which is the only place this
        // reading survives; `parse_end` replaces it when the cumulative
        // `end` payload lands here.
        context_tokens: field("input_tokens")
            .saturating_add(field("cache_read_input_tokens"))
            .saturating_add(field("cache_creation_input_tokens")),
        first_call_context_tokens: None,
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
        // reason is the opposite: it ends the turn as a visible failure.
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
            Some(HarnessEvent::TurnFailed { .. })
        ));
    }

    #[test]
    fn a_missing_stop_reason_fails_closed() {
        let out = GrokStreamParser::parse_ndjson(
            r#"{"type":"end","sessionId":"abc","usage":{"input_tokens":1}}"#,
        );

        assert_eq!(out.unrecognized, 1);
        assert!(matches!(
            out.events.last(),
            Some(HarnessEvent::TurnFailed { .. })
        ));
    }

    #[test]
    fn progress_status_synonyms_do_not_complete_or_count_a_tool() {
        for status in ["in_progress", "inProgress", "running"] {
            let input = format!(
                "{{\"type\":\"tool_call\",\"toolCallId\":\"call-1\",\"toolName\":\"read_file\",\"rawInput\":{{}}}}\n{{\"type\":\"tool_call_update\",\"toolCallId\":\"call-1\",\"status\":\"{status}\"}}"
            );
            let out = GrokStreamParser::parse_ndjson(&input);
            assert_eq!(out.unrecognized, 0, "status: {status}");
            assert!(!out
                .events
                .iter()
                .any(|event| matches!(event, HarnessEvent::ToolCompleted { .. })));
        }
    }

    #[test]
    fn usage_keeps_the_first_and_last_call_contexts() {
        let input = r#"
{"type":"usage","usage":{"input_tokens":9000,"cache_read_input_tokens":1000}}
{"type":"usage","usage":{"input_tokens":100,"cache_read_input_tokens":12000}}
{"type":"end","stopReason":"end_turn","sessionId":"abc","usage":{"input_tokens":9100,"cache_read_input_tokens":13000}}
"#;
        let out = GrokStreamParser::parse_ndjson(input);
        let usage = out.events.iter().find_map(|event| match event {
            HarnessEvent::TurnCompleted { usage } => Some(usage),
            _ => None,
        });
        assert_eq!(
            usage.and_then(|usage| usage.first_call_context_tokens),
            Some(10_000)
        );
        assert_eq!(usage.map(|usage| usage.context_tokens), Some(12_100));
    }

    #[test]
    fn a_single_unkeyed_kill_result_settles_the_addressed_subagent() {
        let input = r#"
{"type":"tool_call","toolCallId":"call-kill","toolName":"kill_command_or_subagent","rawInput":{"task_id":"01a02025-bcce-7723-a8f6-27e6f2a6a856"}}
{"type":"tool_call_update","toolCallId":"call-kill","status":"completed","rawOutput":{"Result":{"outcome":"killed"}}}
"#;
        let out = GrokStreamParser::parse_ndjson(input);
        assert!(out.events.iter().any(|event| matches!(
            event,
            HarnessEvent::ToolCompleted {
                call_id,
                outcome: ToolOutcome::Failed,
                parent_call_id: None,
                ..
            } if call_id == "01a02025-bcce-7723-a8f6-27e6f2a6a856"
        )));
    }
}
