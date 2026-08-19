//! Parse captured opencode serve HTTP + SSE frames into [`HarnessEvent`]s.
//!
//! Written only against the checked-in fixtures under
//! `fixtures/opencode/1.18.18/`. Unknown event types increment a counter
//! and are logged (size-capped). They are never fatal and never dropped
//! silently.
//!
//! Fixture lines are framed `{ "dir": "in"|"out", "msg": { "kind": ... } }`:
//!
//! ```text
//! {"dir":"out","msg":{"kind":"http","method":"POST","path":"/session","body":{…}}}
//! {"dir":"in","msg":{"kind":"http","status":200,"path":"/session","body":{…}}}
//! {"dir":"in","msg":{"kind":"sse","event":{"type":"session.created",…}}}
//! ```

use std::collections::HashSet;

use serde_json::Value;
use tidebreak_core::{
    BoundedError, CodeUsage, HarnessKind, ToolDetail, ToolOutcome, MAX_EVENT_TEXT_CHARS,
    MAX_NOTICE_CHARS, MAX_PREVIEW_CHARS,
};

use crate::{ApprovalDecision, HarnessApprovalRef, HarnessEvent};

/// Longest unrecognized payload kept for the debug log.
const MAX_UNRECOGNIZED_LOG: usize = 512;

/// Incremental parser for one opencode serve HTTP + SSE exchange.
#[derive(Debug, Default)]
pub struct OpencodeStreamParser {
    unrecognized: u64,
    resume_ref: Option<String>,
    version: Option<String>,
    emitted_session: bool,
    turn_open: bool,
    turn_terminal: bool,
    started_tools: HashSet<String>,
    completed_tools: HashSet<String>,
    resolved_approvals: HashSet<String>,
    last_usage: CodeUsage,
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

impl OpencodeStreamParser {
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

    /// Parse one framed NDJSON line. Never returns an error: unknown shapes
    /// increment [`Self::unrecognized`].
    pub fn push_line(&mut self, line: &str) -> Vec<HarnessEvent> {
        let line = line.trim();
        if line.is_empty() {
            return Vec::new();
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            self.count_unrecognized("unparseable-line", line);
            return Vec::new();
        };
        if let (Some(dir), Some(msg)) = (value.get("dir").and_then(Value::as_str), value.get("msg"))
        {
            return match dir {
                "out" => self.push_outbound(msg),
                "in" => self.push_inbound(msg),
                other => {
                    self.count_unrecognized(&format!("frame/{other}"), &value);
                    Vec::new()
                }
            };
        }
        self.push_inbound(&value)
    }

    /// Parse a whole captured framed NDJSON document.
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

    /// Parse one inbound SSE event object.
    pub fn push_sse(&mut self, event: &Value) -> Vec<HarnessEvent> {
        self.push_inbound(&serde_json::json!({ "kind": "sse", "event": event }))
    }

    fn push_outbound(&mut self, value: &Value) -> Vec<HarnessEvent> {
        let kind = value.get("kind").and_then(Value::as_str).unwrap_or("");
        if kind != "http" {
            self.count_unrecognized(&format!("out/{kind}"), value);
            return Vec::new();
        }
        let method = value.get("method").and_then(Value::as_str).unwrap_or("");
        let path = value.get("path").and_then(Value::as_str).unwrap_or("");
        if method == "POST" && path.contains("/prompt_async") {
            self.turn_terminal = false;
            return Vec::new();
        }
        if method == "POST" && path.contains("/permission/") && path.ends_with("/reply") {
            return self.parse_permission_reply(path, value.get("body"));
        }
        if method == "POST" && path.ends_with("/abort") {
            return Vec::new();
        }
        if matches!(method, "POST" | "GET") && is_session_root_or_get(path) {
            return Vec::new();
        }
        self.count_unrecognized(&format!("out/http/{method}{path}"), value);
        Vec::new()
    }

    fn push_inbound(&mut self, value: &Value) -> Vec<HarnessEvent> {
        match value.get("kind").and_then(Value::as_str) {
            Some("http") => self.parse_http_in(value),
            Some("sse") => {
                let event = value.get("event").cloned().unwrap_or(Value::Null);
                self.parse_sse(&event)
            }
            Some(other) => {
                self.count_unrecognized(&format!("in/{other}"), value);
                Vec::new()
            }
            None => {
                if value.get("type").is_some() {
                    return self.parse_sse(value);
                }
                self.count_unrecognized("in/untyped", value);
                Vec::new()
            }
        }
    }

    fn parse_http_in(&mut self, value: &Value) -> Vec<HarnessEvent> {
        let path = value.get("path").and_then(Value::as_str).unwrap_or("");
        let status = value.get("status").and_then(Value::as_u64).unwrap_or(0);
        let body = value.get("body").cloned().unwrap_or(Value::Null);
        if status == 204 {
            return Vec::new();
        }
        if path.contains("/permission/") && path.ends_with("/reply") {
            return Vec::new();
        }
        if path.ends_with("/abort") {
            return Vec::new();
        }
        if is_session_root_or_get(path) && (200..300).contains(&status) {
            return self.emit_session_started(&body);
        }
        if (200..300).contains(&status) {
            return Vec::new();
        }
        if status >= 400 {
            let message = body
                .pointer("/data/message")
                .or_else(|| body.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("engine reported an error");
            return vec![HarnessEvent::TurnFailed {
                error: BoundedError {
                    message: bound(message, MAX_NOTICE_CHARS),
                },
            }];
        }
        self.count_unrecognized(&format!("in/http/{status}{path}"), value);
        Vec::new()
    }

    fn parse_sse(&mut self, event: &Value) -> Vec<HarnessEvent> {
        let Some(kind) = event.get("type").and_then(Value::as_str) else {
            self.count_unrecognized("sse/missing-type", event);
            return Vec::new();
        };
        let props = event.get("properties").cloned().unwrap_or(Value::Null);
        match kind {
            "session.created" => {
                let info = props.get("info").cloned().unwrap_or(Value::Null);
                self.emit_session_started(&info)
            }
            "session.status" => self.parse_session_status(&props),
            "session.idle" => self.emit_turn_completed(),
            "session.error" => self.parse_session_error(&props),
            "message.part.delta" => self.parse_part_delta(&props),
            "message.part.updated" => self.parse_part_updated(&props),
            "message.updated" => {
                self.harvest_usage(props.get("info"));
                Vec::new()
            }
            "permission.asked" => self.parse_permission_asked(&props),
            "permission.replied" => self.parse_permission_replied(&props),
            "server.connected" | "server.heartbeat" => Vec::new(),
            other => {
                self.count_unrecognized(other, event);
                Vec::new()
            }
        }
    }

    fn parse_session_status(&mut self, props: &Value) -> Vec<HarnessEvent> {
        let status = props
            .pointer("/status/type")
            .and_then(Value::as_str)
            .unwrap_or("");
        match status {
            "busy" => {
                if self.turn_open {
                    return Vec::new();
                }
                self.turn_open = true;
                self.turn_terminal = false;
                vec![HarnessEvent::TurnStarted]
            }
            "idle" => self.emit_turn_completed(),
            other => {
                self.count_unrecognized(&format!("session.status/{other}"), props);
                Vec::new()
            }
        }
    }

    fn parse_session_error(&mut self, props: &Value) -> Vec<HarnessEvent> {
        let error = props.get("error").cloned().unwrap_or(Value::Null);
        let name = error.get("name").and_then(Value::as_str).unwrap_or("");
        let message = error
            .pointer("/data/message")
            .and_then(Value::as_str)
            .unwrap_or("engine reported an error");
        if self.turn_terminal {
            return Vec::new();
        }
        self.turn_terminal = true;
        self.turn_open = false;
        if name == "MessageAbortedError" {
            return vec![HarnessEvent::TurnInterrupted];
        }
        vec![HarnessEvent::TurnFailed {
            error: BoundedError {
                message: bound(message, MAX_NOTICE_CHARS),
            },
        }]
    }

    fn emit_turn_completed(&mut self) -> Vec<HarnessEvent> {
        if self.turn_terminal {
            return Vec::new();
        }
        self.turn_terminal = true;
        self.turn_open = false;
        vec![HarnessEvent::TurnCompleted {
            usage: self.last_usage.clone(),
        }]
    }

    fn parse_part_delta(&mut self, props: &Value) -> Vec<HarnessEvent> {
        let field = props.get("field").and_then(Value::as_str).unwrap_or("");
        let text = props.get("delta").and_then(Value::as_str).unwrap_or("");
        if text.is_empty() {
            return Vec::new();
        }
        match field {
            "text" => vec![HarnessEvent::AssistantDelta {
                text: bound(text, MAX_EVENT_TEXT_CHARS),
            }],
            "reasoning" => vec![HarnessEvent::ReasoningDelta {
                text: bound(text, MAX_EVENT_TEXT_CHARS),
            }],
            other => {
                self.count_unrecognized(&format!("message.part.delta/{other}"), props);
                Vec::new()
            }
        }
    }

    fn parse_part_updated(&mut self, props: &Value) -> Vec<HarnessEvent> {
        let part = props.get("part").cloned().unwrap_or(Value::Null);
        match part.get("type").and_then(Value::as_str) {
            Some("text") => {
                let text = part.get("text").and_then(Value::as_str).unwrap_or("");
                let complete = part.pointer("/time/end").is_some();
                if text.is_empty() || !complete {
                    Vec::new()
                } else {
                    vec![HarnessEvent::AssistantMessage {
                        text: bound(text, MAX_EVENT_TEXT_CHARS),
                    }]
                }
            }
            Some("reasoning") => {
                let text = part.get("text").and_then(Value::as_str).unwrap_or("");
                if text.is_empty() {
                    Vec::new()
                } else {
                    vec![HarnessEvent::ReasoningDelta {
                        text: bound(text, MAX_EVENT_TEXT_CHARS),
                    }]
                }
            }
            Some("tool") => self.parse_tool_part(&part),
            Some("step-finish") => {
                self.harvest_usage(Some(&part));
                Vec::new()
            }
            Some("step-start") => Vec::new(),
            Some(other) => {
                self.count_unrecognized(&format!("message.part.updated/{other}"), &part);
                Vec::new()
            }
            None => {
                self.count_unrecognized("message.part.updated/untyped", &part);
                Vec::new()
            }
        }
    }

    fn parse_tool_part(&mut self, part: &Value) -> Vec<HarnessEvent> {
        let call_id = part
            .get("callID")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        if call_id.is_empty() {
            self.count_unrecognized("tool/missing-callID", part);
            return Vec::new();
        }
        let name = part
            .get("tool")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned();
        let state = part.get("state").cloned().unwrap_or(Value::Null);
        let status = state.get("status").and_then(Value::as_str).unwrap_or("");
        let input = state.get("input").cloned().unwrap_or(Value::Null);
        match status {
            "pending" | "running" => {
                if !self.started_tools.insert(call_id.clone()) {
                    return Vec::new();
                }
                vec![HarnessEvent::ToolStarted {
                    call_id,
                    name: name.clone(),
                    detail: tool_detail(&name, &input),
                }]
            }
            "completed" | "error" => {
                if !self.completed_tools.insert(call_id.clone()) {
                    return Vec::new();
                }
                let mut events = Vec::new();
                if self.started_tools.insert(call_id.clone()) {
                    events.push(HarnessEvent::ToolStarted {
                        call_id: call_id.clone(),
                        name: name.clone(),
                        detail: tool_detail(&name, &input),
                    });
                }
                let error = state.get("error").and_then(Value::as_str).unwrap_or("");
                let outcome = if status == "error" {
                    if error.to_ascii_lowercase().contains("rejected permission") {
                        ToolOutcome::Denied
                    } else {
                        ToolOutcome::Failed
                    }
                } else {
                    ToolOutcome::Succeeded
                };
                let preview = state
                    .pointer("/metadata/preview")
                    .and_then(Value::as_str)
                    .or_else(|| state.get("output").and_then(Value::as_str))
                    .or(if error.is_empty() { None } else { Some(error) })
                    .unwrap_or("");
                // The `pending` part that opens the call carries `input: {}`;
                // the arguments land on the `running` and terminal parts. The
                // terminal part is the correction for the call already
                // started, and the detail above for one that was not.
                let detail = tool_detail(&name, &input);
                events.push(HarnessEvent::ToolCompleted {
                    call_id,
                    outcome,
                    preview: bound(preview, MAX_PREVIEW_CHARS),
                    detail: (detail.specificity() > 0).then_some(detail),
                });
                events
            }
            other => {
                self.count_unrecognized(&format!("tool/status/{other}"), part);
                Vec::new()
            }
        }
    }

    fn parse_permission_asked(&mut self, props: &Value) -> Vec<HarnessEvent> {
        let call_id = props
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        if call_id.is_empty() {
            self.count_unrecognized("permission.asked/missing-id", props);
            return Vec::new();
        }
        vec![HarnessEvent::ApprovalRequested {
            harness_ref: HarnessApprovalRef { call_id },
            raw: serde_json::Value::Null,
        }]
    }

    fn parse_permission_reply(&mut self, path: &str, body: Option<&Value>) -> Vec<HarnessEvent> {
        let call_id = permission_id_from_path(path).unwrap_or_default();
        if call_id.is_empty() {
            self.count_unrecognized("permission.reply/missing-id", path);
            return Vec::new();
        }
        if !self.resolved_approvals.insert(call_id.clone()) {
            return Vec::new();
        }
        let reply = body
            .and_then(|value| value.get("reply"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let decision = match reply {
            "once" | "always" => ApprovalDecision::Approve,
            "reject" => ApprovalDecision::Deny {
                feedback: body
                    .and_then(|value| value.get("message"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            },
            other => {
                self.count_unrecognized(
                    &format!("permission.reply/{other}"),
                    body.unwrap_or(&Value::Null),
                );
                return Vec::new();
            }
        };
        vec![HarnessEvent::ApprovalResolved {
            harness_ref: HarnessApprovalRef { call_id },
            decision,
        }]
    }

    fn parse_permission_replied(&mut self, props: &Value) -> Vec<HarnessEvent> {
        let call_id = props
            .get("requestID")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        if call_id.is_empty() {
            self.count_unrecognized("permission.replied/missing-id", props);
            return Vec::new();
        }
        if !self.resolved_approvals.insert(call_id.clone()) {
            return Vec::new();
        }
        let reply = props.get("reply").and_then(Value::as_str).unwrap_or("");
        let decision = match reply {
            "once" | "always" => ApprovalDecision::Approve,
            "reject" => ApprovalDecision::Deny { feedback: None },
            other => {
                self.count_unrecognized(&format!("permission.replied/{other}"), props);
                return Vec::new();
            }
        };
        vec![HarnessEvent::ApprovalResolved {
            harness_ref: HarnessApprovalRef { call_id },
            decision,
        }]
    }

    fn emit_session_started(&mut self, info: &Value) -> Vec<HarnessEvent> {
        if let Some(id) = info.get("id").and_then(Value::as_str) {
            self.resume_ref = Some(id.to_owned());
        }
        if let Some(version) = info.get("version").and_then(Value::as_str) {
            self.version = Some(version.to_owned());
        }
        if self.emitted_session {
            return Vec::new();
        }
        if self.resume_ref.is_none() {
            return Vec::new();
        }
        self.emitted_session = true;
        vec![HarnessEvent::SessionStarted {
            harness_kind: HarnessKind::Opencode,
            harness_version: self.version.clone().unwrap_or_else(|| "unknown".into()),
            resume_ref: self.resume_ref.clone(),
        }]
    }

    fn harvest_usage(&mut self, info: Option<&Value>) {
        let Some(info) = info else {
            return;
        };
        let tokens = info.get("tokens").unwrap_or(info);
        if tokens.get("input").is_none() && tokens.get("output").is_none() {
            return;
        }
        self.last_usage = CodeUsage {
            input_tokens: tokens.get("input").and_then(Value::as_u64).unwrap_or(0),
            output_tokens: tokens.get("output").and_then(Value::as_u64).unwrap_or(0),
            cache_read_input_tokens: tokens
                .pointer("/cache/read")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            cache_creation_input_tokens: tokens
                .pointer("/cache/write")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        };
    }

    fn count_unrecognized(&mut self, label: &str, payload: impl std::fmt::Display) {
        self.unrecognized += 1;
        let mut rendered = payload.to_string();
        crate::text::truncate_on_char_boundary(&mut rendered, MAX_UNRECOGNIZED_LOG);
        tracing::debug!(
            target: "tidebreak_harness::opencode",
            unrecognized = self.unrecognized,
            kind = label,
            payload = %rendered,
            "unrecognized engine event"
        );
    }
}

fn is_session_root_or_get(path: &str) -> bool {
    if path == "/session" {
        return true;
    }
    let rest = path.strip_prefix("/session/").unwrap_or("");
    !rest.is_empty() && !rest.contains('/')
}

fn permission_id_from_path(path: &str) -> Option<String> {
    // /permission/{id}/reply
    let mut parts = path.split('/');
    if parts.next() != Some("") {
        return None;
    }
    if parts.next() != Some("permission") {
        return None;
    }
    let id = parts.next()?;
    if parts.next() != Some("reply") {
        return None;
    }
    if id.is_empty() {
        return None;
    }
    Some(id.to_owned())
}

fn tool_detail(name: &str, input: &Value) -> ToolDetail {
    match name {
        "read" => ToolDetail::FileRead {
            path: input
                .get("filePath")
                .or_else(|| input.get("path"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
        },
        "edit" | "write" => ToolDetail::FileEdit {
            path: input
                .get("filePath")
                .or_else(|| input.get("path"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
        },
        "bash" => ToolDetail::Command {
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
        "glob" | "grep" => ToolDetail::Search {
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
{"dir":"in","msg":{"kind":"http","status":200,"path":"/session","body":{"id":"ses_abc","version":"1.18.18"}}}
{"dir":"in","msg":{"kind":"sse","event":{"type":"brand_new_shape","properties":{"foo":1}}}}
{"dir":"in","msg":{"kind":"sse","event":{"type":"session.status","properties":{"status":{"type":"busy"}}}}}
{"dir":"in","msg":{"kind":"sse","event":{"type":"session.idle","properties":{"sessionID":"ses_abc"}}}}
"#;
        let out = OpencodeStreamParser::parse_ndjson(input);
        assert!(out.unrecognized >= 1);
        assert!(matches!(
            out.events.first(),
            Some(HarnessEvent::SessionStarted { .. })
        ));
        assert!(matches!(
            out.events.last(),
            Some(HarnessEvent::TurnCompleted { .. })
        ));
        assert_eq!(out.resume_ref.as_deref(), Some("ses_abc"));
    }
}
