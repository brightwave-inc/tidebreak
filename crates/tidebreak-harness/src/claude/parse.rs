//! Parse captured Claude Code `stream-json` lines into [`HarnessEvent`]s.
//!
//! Written only against the checked-in fixtures under
//! `fixtures/claude-code/2.1.233/`. Unknown event types increment a counter
//! and are logged (size-capped). They are never fatal and never dropped
//! silently.

use std::collections::{HashMap, HashSet};

use serde_json::Value;
use tidebreak_core::{
    BoundedError, HarnessKind, HarnessNoticeLevel, ToolDetail, ToolOutcome, TurnUsage,
    MAX_EVENT_TEXT_CHARS, MAX_NOTICE_CHARS, MAX_PREVIEW_CHARS,
};

use crate::oversized::{CutLine, OVERSIZED_PAYLOAD};
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
    /// call id → the `Task` call it runs inside, for every call started and
    /// not yet completed. A result the line buffer cut loses its line's
    /// `parent_tool_use_id`, and sometimes its own `tool_use_id`.
    running_calls: HashMap<String, Option<String>>,
    /// message id → the `Task` call the message streams inside, for the
    /// current turn. A cut `assistant` line loses its attribution the same
    /// way a cut result does.
    message_parents: HashMap<String, Option<String>>,
    /// Tasks the engine is running, by task id. A task in the background
    /// settles its tool call at once with a placeholder, so its real end
    /// arrives only as a notification.
    tasks: HashMap<String, EngineTask>,
    emitted_session: bool,
    reported_model: Option<String>,
}

/// Which content block a stream event belongs to.
///
/// Two subagents stream at once (decision 52) and each numbers its blocks
/// from zero, so the index alone does not identify one. The `Task` call the
/// lines run inside separates them.
type BlockKey = (Option<String>, u64);

/// A task the engine is running for a tool call.
#[derive(Debug, Clone, Default)]
struct EngineTask {
    /// A subagent, rather than a command or a tool.
    subagent: bool,
    /// The engine's one-line description of the task.
    description: String,
    /// Running in the background, where its tool result is a placeholder.
    background: bool,
}

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

    /// Parse one line the line buffer cut at its cap.
    ///
    /// The part that arrived still names the event. A tool result settles its
    /// call with [`OVERSIZED_PAYLOAD`] in place of the output the cut took,
    /// and a turn's closing `result` still closes the turn.
    pub fn push_cut_line(&mut self, line: &str) -> Vec<HarnessEvent> {
        let Some(cut) = CutLine::recover(line) else {
            self.count_unrecognized("oversized-line", line);
            return Vec::new();
        };
        let result_cut = cut.cut_within(&["result"]);
        let content_index = cut.index_under(&["message", "content"]);
        let text_cut = content_index.is_some() && cut.cut_within(&["message", "content", "text"]);
        let CutLine {
            mut value,
            cut_text,
            ..
        } = cut;
        // A `result` line names its type after the final text, so a cut in
        // that text leaves a line only `result` itself identifies.
        if result_cut && value.get("type").is_none() {
            value["type"] = Value::String("result".into());
        }
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        tracing::info!(
            target: "tidebreak_harness::claude",
            kind = kind.as_str(),
            "engine line exceeded the parse budget; kept the part that arrived"
        );
        // Text a person reads keeps what arrived, bounded like any other.
        if let (true, Some(index)) = (text_cut, content_index) {
            if let Some(block) = value.pointer_mut(&format!("/message/content/{index}")) {
                block["text"] = Value::String(cut_text.clone());
            }
        }
        match kind.as_str() {
            "user" => {
                if let Some(index) = content_index {
                    self.settle_cut_result(&mut value, index);
                }
            }
            "assistant" => {
                if value.get("parent_tool_use_id").is_none() {
                    let parent = value
                        .pointer("/message/id")
                        .and_then(Value::as_str)
                        .and_then(|id| self.message_parents.get(id))
                        .cloned()
                        .flatten();
                    value["parent_tool_use_id"] = parent.map_or(Value::Null, Value::String);
                }
            }
            "result" if result_cut => {
                value["result"] = Value::String(cut_text);
            }
            _ => {}
        }
        self.push_value(&value)
    }

    /// Point the tool result the cut landed in at its call, and put
    /// [`OVERSIZED_PAYLOAD`] where its output was.
    ///
    /// A successful result names its `tool_use_id` before its content; an
    /// error names it, and `is_error`, after (captured on 2.1.233). With the
    /// id cut away the result is an error, and it belongs to the one call
    /// still running with nothing running inside it, when exactly one is. The
    /// line cannot say which of several it was.
    fn settle_cut_result(&mut self, value: &mut Value, index: usize) {
        let Some(block) = value.pointer_mut(&format!("/message/content/{index}")) else {
            return;
        };
        if block.get("type").and_then(Value::as_str) != Some("tool_result") {
            return;
        }
        if block.get("tool_use_id").and_then(Value::as_str).is_none() {
            let spans: HashSet<&str> = self
                .running_calls
                .values()
                .filter_map(|parent| parent.as_deref())
                .collect();
            let mut leaves = self
                .running_calls
                .keys()
                .filter(|call_id| !spans.contains(call_id.as_str()));
            match (leaves.next(), leaves.next()) {
                (Some(call_id), None) => {
                    block["tool_use_id"] = Value::String(call_id.clone());
                    block["is_error"] = Value::Bool(true);
                }
                _ => {
                    self.count_unrecognized("oversized-line/tool_result", "no tool_use_id");
                    return;
                }
            }
        }
        block["content"] = Value::String(OVERSIZED_PAYLOAD.to_owned());
        let call_id = block
            .get("tool_use_id")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if value.get("parent_tool_use_id").is_none() {
            let parent = call_id
                .and_then(|id| self.running_calls.get(&id).cloned())
                .flatten();
            value["parent_tool_use_id"] = parent.map_or(Value::Null, Value::String);
        }
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
            // A heartbeat every 30 seconds while a tool runs (captured on
            // 2.1.259). The call's card already shows it running and times
            // it, and the heartbeat's own `tool_use_id` names no call.
            "tool_progress" => Vec::new(),
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
                if parent_call_id(value).is_some() {
                    return Vec::new();
                }
                if let Some(session_id) = value.get("session_id").and_then(Value::as_str) {
                    self.resume_ref = Some(session_id.to_owned());
                }
                if let Some(version) = value.get("claude_code_version").and_then(Value::as_str) {
                    self.version = Some(version.to_owned());
                }
                let mut events = self.report_model(value.get("model"));
                if self.emitted_session {
                    return events;
                }
                self.emitted_session = true;
                events.insert(
                    0,
                    HarnessEvent::SessionStarted {
                        harness_kind: HarnessKind::ClaudeCode,
                        harness_version: self.version.clone().unwrap_or_else(|| "unknown".into()),
                        resume_ref: self.resume_ref.clone(),
                    },
                );
                events
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
            // The task kinds below were captured on 2.1.259. A task rides on
            // the tool call that started it: a Bash call that runs for more
            // than a moment, one run in the background, or a subagent. That
            // call's card or span is already in the transcript.
            "task_started" => {
                self.note_task_started(value);
                Vec::new()
            }
            "task_updated" => {
                // A patch to the task's state. Only a move to the background
                // changes anything here: the task's end then arrives as its
                // notification rather than its tool result.
                if value
                    .pointer("/patch/is_backgrounded")
                    .and_then(Value::as_bool)
                    == Some(true)
                {
                    if let Some(task) = value
                        .get("task_id")
                        .and_then(Value::as_str)
                        .and_then(|task_id| self.tasks.get_mut(task_id))
                    {
                        task.background = true;
                    }
                }
                Vec::new()
            }
            "task_notification" => self.parse_task_notification(value),
            // A subagent's running token and tool counts. Its own calls
            // already stream into its span.
            "task_progress"
            // The whole set of background tasks, restated on every change.
            // `task_started` and `task_notification` carry the same edges.
            | "background_tasks_changed"
            // A commit, push, or pull request a Bash call just made. The
            // call's card shows the command and its output, and Tidebreak
            // reads repository and pull-request state from the repository
            // and the forge, not from the engine's hint.
            | "vcs_state_changed"
            | "code_change_published" => Vec::new(),
            other => {
                self.count_unrecognized(&format!("system/{other}"), value);
                Vec::new()
            }
        }
    }

    /// Remember a task, to know at its end whether its tool result already
    /// reported it.
    ///
    /// A task in the background settles its tool call at once with a
    /// placeholder: "Command running in background with ID: …" for a
    /// command, "Async agent launched successfully" for a subagent. A
    /// housekeeping task the engine marks `skip_transcript` or `ambient` is
    /// not the person's work, so it is not followed.
    fn note_task_started(&mut self, value: &Value) {
        let flag = |key: &str| value.get(key).and_then(Value::as_bool) == Some(true);
        if flag("skip_transcript") || flag("ambient") {
            return;
        }
        let Some(task_id) = value.get("task_id").and_then(Value::as_str) else {
            return;
        };
        self.tasks.insert(
            task_id.to_owned(),
            EngineTask {
                subagent: value.get("task_type").and_then(Value::as_str) == Some("local_agent"),
                description: value
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_owned(),
                background: flag("is_backgrounded"),
            },
        );
    }

    /// A task finished. A foreground task's own tool result follows and
    /// reports it. A background task's call settled long ago with a
    /// placeholder, so its end reaches the transcript as a notice. A
    /// command's notice is the engine's own summary: `Background command "…"
    /// completed (exit code 0)`. A subagent's summary is its whole final
    /// answer, which its span already shows, so its notice just names it.
    fn parse_task_notification(&mut self, value: &Value) -> Vec<HarnessEvent> {
        let Some(task_id) = value.get("task_id").and_then(Value::as_str) else {
            self.count_unrecognized("system/task_notification/missing-task", value);
            return Vec::new();
        };
        let Some(task) = self.tasks.remove(task_id).filter(|task| task.background) else {
            return Vec::new();
        };
        let status = value.get("status").and_then(Value::as_str).unwrap_or("");
        let summary = value
            .get("summary")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|summary| !summary.is_empty());
        let message = match (task.subagent, summary) {
            (false, Some(summary)) => summary.to_owned(),
            (subagent, _) => {
                let what = match (subagent, task.description.is_empty()) {
                    (true, true) => "A background subagent".to_owned(),
                    (true, false) => format!("Subagent \"{}\"", task.description),
                    (false, true) => "A background task".to_owned(),
                    (false, false) => format!("Background task \"{}\"", task.description),
                };
                match status {
                    "completed" => format!("{what} finished."),
                    "failed" => format!("{what} failed."),
                    "stopped" => format!("{what} was stopped."),
                    _ => format!("{what} ended."),
                }
            }
        };
        vec![HarnessEvent::HarnessNotice {
            level: if status == "completed" {
                HarnessNoticeLevel::Info
            } else {
                HarnessNoticeLevel::Warning
            },
            message: bound(&message, MAX_NOTICE_CHARS),
        }]
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
            "message_start" => {
                if let Some(id) = event.pointer("/message/id").and_then(Value::as_str) {
                    self.message_parents
                        .insert(id.to_owned(), parent_call_id(value));
                }
                Vec::new()
            }
            "message_delta" | "message_stop" | "ping" => Vec::new(),
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
        if parent.is_none() {
            events.extend(self.report_model(value.pointer("/message/model")));
        }
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

    fn report_model(&mut self, value: Option<&Value>) -> Vec<HarnessEvent> {
        let Some(model) = value
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|model| {
                !model.is_empty()
                    && model.encode_utf16().count() <= 160
                    && !model.chars().any(char::is_control)
            })
        else {
            return Vec::new();
        };
        if self.reported_model.as_deref() == Some(model) {
            return Vec::new();
        }
        self.reported_model = Some(model.to_owned());
        vec![HarnessEvent::ModelReported {
            model: model.to_owned(),
        }]
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
                        self.running_calls.remove(&call_id);
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
                            correlation_uuid: None,
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
        // Messages and tool calls never span turns: a call the turn left
        // without a result will not get one now.
        self.message_parents.clear();
        self.running_calls.clear();
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
        self.running_calls.insert(call_id.clone(), parent.clone());
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
///
/// 2.1.259 names its subagent tool `Agent` (captured); 2.1.233 called it
/// `Task`. The span every adapter emits for a subagent is `Task` (decision
/// 52), and the rail and the transcript look for that name.
fn tool_name(block: &Value) -> String {
    match block
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
    {
        "Agent" => "Task".to_owned(),
        name => name.to_owned(),
    }
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

fn usage_from(value: Option<&Value>) -> TurnUsage {
    let Some(value) = value else {
        return TurnUsage::default();
    };
    TurnUsage {
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

    #[test]
    fn reports_the_parent_model_without_borrowing_a_child_model() {
        let out = ClaudeStreamParser::parse_ndjson(
            r#"
{"type":"system","subtype":"init","session_id":"parent","model":"claude-default"}
{"type":"system","subtype":"init","session_id":"child","parent_tool_use_id":"child-task","model":"child-init-model"}
{"type":"assistant","parent_tool_use_id":"child","message":{"model":"child-model","content":[]}}
{"type":"assistant","message":{"model":"claude-default","content":[]}}
{"type":"assistant","message":{"model":"claude-effective","content":[]}}
{"type":"assistant","message":{"model":"invalid\nmodel","content":[]}}
"#,
        );
        let models: Vec<_> = out
            .events
            .iter()
            .filter_map(|event| match event {
                HarnessEvent::ModelReported { model } => Some(model.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(models, ["claude-default", "claude-effective"]);
        assert_eq!(out.resume_ref.as_deref(), Some("parent"));
    }

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

    /// Cut `line` where the 256 KiB session budget cuts it.
    fn cut(line: &str) -> String {
        let cut = crate::oversized::through_the_line_buffer(
            line,
            crate::budget::DEFAULT_MAX_PARTIAL_LINE,
        );
        assert!(cut.cut, "the line must outgrow the budget");
        cut.text
    }

    fn huge() -> String {
        "A".repeat(crate::budget::DEFAULT_MAX_PARTIAL_LINE)
    }

    fn start_read(parser: &mut ClaudeStreamParser, call_id: &str, parent: Option<&str>) {
        let line = serde_json::json!({
            "type": "assistant",
            "message": {"id": format!("msg_{call_id}"), "content": [
                {"type": "tool_use", "id": call_id, "name": "Read", "input": {"file_path": "big.png"}}
            ]},
            "parent_tool_use_id": parent,
        });
        assert_eq!(parser.push_line(&line.to_string()).len(), 1);
    }

    /// An image result over the line budget used to fail to parse and vanish,
    /// which left its tool card running for the rest of the session.
    #[test]
    fn a_tool_result_over_the_line_budget_settles_its_call() {
        let mut parser = ClaudeStreamParser::new();
        start_read(&mut parser, "toolu_image", None);
        let line = format!(
            r#"{{"type":"user","message":{{"role":"user","content":[{{"tool_use_id":"toolu_image","type":"tool_result","content":[{{"type":"image","source":{{"type":"base64","media_type":"image/png","data":"{}"}}}}]}}]}},"parent_tool_use_id":null,"session_id":"s"}}"#,
            huge()
        );
        let events = parser.push_cut_line(&cut(&line));
        assert_eq!(
            events,
            vec![HarnessEvent::ToolCompleted {
                call_id: "toolu_image".into(),
                outcome: ToolOutcome::Succeeded,
                preview: OVERSIZED_PAYLOAD.into(),
                detail: None,
                parent_call_id: None,
            }]
        );
        assert_eq!(parser.unrecognized(), 0);
    }

    /// An error result names its call after its content, and the line names
    /// its subagent after the whole message. With both cut away, the one
    /// call still running with nothing inside it is the call the result
    /// answers, under that call's own subagent, and it failed.
    #[test]
    fn a_cut_result_that_lost_its_ids_settles_the_one_running_call() {
        let mut parser = ClaudeStreamParser::new();
        let task = r#"{"type":"assistant","message":{"id":"msg_task","content":[{"type":"tool_use","id":"toolu_task","name":"Task","input":{"description":"Inspect","prompt":"look"}}]},"parent_tool_use_id":null}"#;
        assert_eq!(parser.push_line(task).len(), 1);
        start_read(&mut parser, "toolu_child", Some("toolu_task"));
        let line = format!(
            r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"tool_result","content":"{}","is_error":true,"tool_use_id":"toolu_child"}}]}},"parent_tool_use_id":"toolu_task"}}"#,
            huge()
        );
        let events = parser.push_cut_line(&cut(&line));
        assert_eq!(
            events,
            vec![HarnessEvent::ToolCompleted {
                call_id: "toolu_child".into(),
                outcome: ToolOutcome::Failed,
                preview: OVERSIZED_PAYLOAD.into(),
                detail: None,
                parent_call_id: Some("toolu_task".into()),
            }]
        );
        assert_eq!(parser.unrecognized(), 0);
    }

    /// Two calls running side by side leave nothing to tell the cut result's
    /// call from the other one. Guessing would settle the wrong card, so the
    /// line is counted instead.
    #[test]
    fn a_cut_result_that_could_answer_either_of_two_calls_is_counted() {
        let mut parser = ClaudeStreamParser::new();
        start_read(&mut parser, "toolu_a", None);
        start_read(&mut parser, "toolu_b", None);
        let line = format!(
            r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"tool_result","content":"{}","is_error":true,"tool_use_id":"toolu_b"}}]}}}}"#,
            huge()
        );
        assert!(parser.push_cut_line(&cut(&line)).is_empty());
        assert_eq!(parser.unrecognized(), 1);
    }

    /// The `result` line names its type after the final text (captured on
    /// 2.1.259). A final answer over the budget used to swallow the turn's
    /// end, so the turn never finished.
    #[test]
    fn a_result_line_over_the_budget_still_ends_the_turn() {
        let mut parser = ClaudeStreamParser::new();
        let line = format!(
            r#"{{"duration_api_ms":2607,"stop_reason":"end_turn","session_id":"s","usage":{{"input_tokens":3,"output_tokens":4}},"terminal_reason":"completed","is_error":false,"num_turns":1,"subtype":"success","result":"{}","type":"result","uuid":"u"}}"#,
            huge()
        );
        let events = parser.push_cut_line(&cut(&line));
        assert!(matches!(
            events.as_slice(),
            [HarnessEvent::TurnCompleted { usage }] if usage.output_tokens == 4
        ));
        assert_eq!(parser.resume_ref(), Some("s"));
    }

    /// A reply over the budget keeps the text that arrived, bounded like any
    /// other message, under the subagent its stream named.
    #[test]
    fn an_assistant_message_over_the_budget_keeps_its_text_and_subagent() {
        let mut parser = ClaudeStreamParser::new();
        let start = r#"{"type":"stream_event","event":{"type":"message_start","message":{"id":"msg_long"}},"parent_tool_use_id":"toolu_task"}"#;
        assert!(parser.push_line(start).is_empty());
        let line = format!(
            r#"{{"type":"assistant","message":{{"id":"msg_long","content":[{{"type":"text","text":"{}"}}]}},"parent_tool_use_id":"toolu_task"}}"#,
            huge()
        );
        let events = parser.push_cut_line(&cut(&line));
        assert!(matches!(
            events.as_slice(),
            [HarnessEvent::AssistantMessage { text, parent_call_id: Some(parent) }]
                if text.chars().count() == MAX_EVENT_TEXT_CHARS
                    && text.chars().all(|ch| ch == 'A')
                    && parent == "toolu_task"
        ));
    }

    #[test]
    fn a_cut_line_that_is_not_json_is_counted() {
        let mut parser = ClaudeStreamParser::new();
        assert!(parser.push_cut_line("not json at all").is_empty());
        assert_eq!(parser.unrecognized(), 1);
    }
}
