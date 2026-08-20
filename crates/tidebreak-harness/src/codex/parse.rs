//! Parse captured Codex app-server JSON-RPC frames into [`HarnessEvent`]s.
//!
//! Written only against the checked-in fixtures under
//! `fixtures/codex/0.147.0/`. Unknown methods increment a counter and are
//! logged (size-capped). They are never fatal and never dropped silently.
//!
//! Fixture lines are framed `{ "dir": "in"|"out", "msg": <json-rpc> }`. A
//! bare JSON-RPC object is treated as inbound.

use std::collections::{HashMap, HashSet};

use serde_json::Value;
use tidebreak_core::{
    BoundedError, CodeUsage, HarnessKind, HarnessNoticeLevel, ToolDetail, ToolOutcome,
    MAX_EVENT_TEXT_CHARS, MAX_NOTICE_CHARS, MAX_PREVIEW_CHARS,
};

use crate::{ApprovalDecision, HarnessApprovalRef, HarnessEvent};

/// Longest unrecognized payload kept for the debug log.
const MAX_UNRECOGNIZED_LOG: usize = 512;

/// Incremental parser for one Codex app-server JSON-RPC stream.
#[derive(Debug, Default)]
pub struct CodexStreamParser {
    unrecognized: u64,
    resume_ref: Option<String>,
    version: Option<String>,
    started_tools: HashSet<String>,
    emitted_session: bool,
    last_usage: CodeUsage,
    last_turn_id: Option<String>,
    outbound_methods: HashMap<String, String>,
    /// itemId → JSON-RPC request id for a parked approval.
    pending_approvals: HashMap<String, Value>,
}

/// Result of parsing a whole fixture or a finished stream.
#[derive(Debug, Clone, PartialEq)]
pub struct ParseOutcome {
    /// Normalized events, in order.
    pub events: Vec<HarnessEvent>,
    /// Count of unknown or unmapped event types.
    pub unrecognized: u64,
    /// Thread id extracted from the stream, when any.
    pub resume_ref: Option<String>,
}

impl CodexStreamParser {
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

    /// Most recent `turnId` seen on the stream.
    #[must_use]
    pub fn last_turn_id(&self) -> Option<&str> {
        self.last_turn_id.as_deref()
    }

    /// JSON-RPC request id for a parked approval `itemId`, when any.
    #[must_use]
    pub fn pending_approval_rpc_id(&self, call_id: &str) -> Option<&Value> {
        self.pending_approvals.get(call_id)
    }

    /// Take the JSON-RPC request id for a parked approval.
    pub fn take_pending_approval(&mut self, call_id: &str) -> Option<Value> {
        self.pending_approvals.remove(call_id)
    }

    /// Record an outbound client request so the matching inbound result can
    /// be classified. Live sessions call this when they write a request.
    pub fn note_outbound(&mut self, id: &Value, method: &str) {
        self.outbound_methods.insert(id_key(id), method.to_owned());
    }

    /// Forget an outbound request that was cancelled or timed out before a
    /// response could be consumed.
    pub fn forget_outbound(&mut self, id: &Value) {
        self.outbound_methods.remove(&id_key(id));
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

    fn push_outbound(&mut self, value: &Value) -> Vec<HarnessEvent> {
        if let (Some(id), Some(method)) =
            (value.get("id"), value.get("method").and_then(Value::as_str))
        {
            self.note_outbound(id, method);
        }
        if let Some(decision) = value.pointer("/result/decision").and_then(Value::as_str) {
            let decision = match decision {
                "accept" | "acceptForSession" | "approved" | "approved_for_session" => {
                    ApprovalDecision::Approve
                }
                "decline" | "cancel" | "abort" => ApprovalDecision::Deny { feedback: None },
                other => {
                    self.count_unrecognized(&format!("decision/{other}"), value);
                    return Vec::new();
                }
            };
            let call_id = self
                .pending_approvals
                .keys()
                .next()
                .cloned()
                .unwrap_or_else(|| value.get("id").map(id_key).unwrap_or_default());
            self.pending_approvals.remove(&call_id);
            return vec![HarnessEvent::ApprovalResolved {
                harness_ref: HarnessApprovalRef { call_id },
                decision,
            }];
        }
        Vec::new()
    }

    fn push_inbound(&mut self, value: &Value) -> Vec<HarnessEvent> {
        if value.get("error").is_some() && value.get("id").is_some() {
            return self.parse_rpc_error(value);
        }
        if let Some(method) = value.get("method").and_then(Value::as_str) {
            return self.parse_method(method, value);
        }
        if value.get("result").is_some() {
            return self.parse_rpc_result(value);
        }
        self.count_unrecognized("inbound/untyped", value);
        Vec::new()
    }

    fn parse_method(&mut self, method: &str, value: &Value) -> Vec<HarnessEvent> {
        let params = value.get("params").cloned().unwrap_or(Value::Null);
        if let Some(turn_id) = params
            .get("turnId")
            .and_then(Value::as_str)
            .or_else(|| params.pointer("/turn/id").and_then(Value::as_str))
        {
            self.last_turn_id = Some(turn_id.to_owned());
        }
        match method {
            "item/agentMessage/delta" => {
                let text = params.get("delta").and_then(Value::as_str).unwrap_or("");
                if text.is_empty() {
                    Vec::new()
                } else {
                    vec![HarnessEvent::AssistantDelta {
                        text: bound(text, MAX_EVENT_TEXT_CHARS),
                    }]
                }
            }
            "item/started" => self.parse_item_started(&params),
            "item/completed" => self.parse_item_completed(&params),
            "item/commandExecution/requestApproval" => self.parse_approval_request(value, &params),
            "turn/started" => vec![HarnessEvent::TurnStarted],
            "turn/completed" => self.parse_turn_completed(&params),
            "warning" | "error" => {
                let message = params
                    .pointer("/message")
                    .and_then(Value::as_str)
                    .or_else(|| params.pointer("/error/message").and_then(Value::as_str))
                    .unwrap_or(method);
                vec![HarnessEvent::HarnessNotice {
                    level: if method == "error" {
                        HarnessNoticeLevel::Error
                    } else {
                        HarnessNoticeLevel::Warning
                    },
                    message: bound(message, MAX_NOTICE_CHARS),
                }]
            }
            "thread/tokenUsage/updated" => {
                self.last_usage = usage_from(params.get("tokenUsage"));
                Vec::new()
            }
            "account/rateLimits/updated"
            | "hook/completed"
            | "hook/started"
            | "mcpServer/startupStatus/updated"
            | "remoteControl/status/changed"
            | "serverRequest/resolved"
            | "thread/started"
            | "thread/status/changed"
            | "thread/goal/cleared" => {
                // Known app-server state notifications that do not change the
                // normalized transcript. Treating them as unknown made every
                // healthy turn look like protocol drift.
                Vec::new()
            }
            other => {
                self.count_unrecognized(other, value);
                Vec::new()
            }
        }
    }

    fn parse_item_started(&mut self, params: &Value) -> Vec<HarnessEvent> {
        let item = params.get("item").cloned().unwrap_or(Value::Null);
        match item.get("type").and_then(Value::as_str) {
            Some("commandExecution") => self.emit_tool_started(&item),
            Some("userMessage" | "agentMessage" | "reasoning") => Vec::new(),
            Some(other) => {
                self.count_unrecognized(&format!("item/started/{other}"), &item);
                Vec::new()
            }
            None => {
                self.count_unrecognized("item/started/untyped", &item);
                Vec::new()
            }
        }
    }

    fn parse_item_completed(&mut self, params: &Value) -> Vec<HarnessEvent> {
        let item = params.get("item").cloned().unwrap_or(Value::Null);
        match item.get("type").and_then(Value::as_str) {
            Some("commandExecution") => self.emit_tool_completed(&item),
            Some("agentMessage") => {
                let text = item.get("text").and_then(Value::as_str).unwrap_or("");
                if text.is_empty() {
                    Vec::new()
                } else {
                    vec![HarnessEvent::AssistantMessage {
                        text: bound(text, MAX_EVENT_TEXT_CHARS),
                    }]
                }
            }
            Some("userMessage" | "reasoning") => Vec::new(),
            Some(other) => {
                self.count_unrecognized(&format!("item/completed/{other}"), &item);
                Vec::new()
            }
            None => {
                self.count_unrecognized("item/completed/untyped", &item);
                Vec::new()
            }
        }
    }

    fn parse_approval_request(&mut self, value: &Value, params: &Value) -> Vec<HarnessEvent> {
        let call_id = params
            .get("itemId")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        if call_id.is_empty() {
            self.count_unrecognized("approval/missing-itemId", value);
            return Vec::new();
        }
        if let Some(id) = value.get("id") {
            self.pending_approvals.insert(call_id.clone(), id.clone());
        }
        vec![HarnessEvent::ApprovalRequested {
            harness_ref: HarnessApprovalRef { call_id },
            raw: params.clone(),
        }]
    }

    fn parse_turn_completed(&mut self, params: &Value) -> Vec<HarnessEvent> {
        let status = params
            .pointer("/turn/status")
            .and_then(Value::as_str)
            .unwrap_or("");
        match status {
            "completed" => vec![HarnessEvent::TurnCompleted {
                usage: self.last_usage.clone(),
            }],
            "interrupted" => vec![HarnessEvent::TurnInterrupted],
            "failed" => {
                let message = params
                    .pointer("/turn/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("engine reported an error");
                vec![HarnessEvent::TurnFailed {
                    error: BoundedError {
                        message: bound(message, MAX_NOTICE_CHARS),
                    },
                }]
            }
            // The manifest records the observed set as
            // `completed | interrupted | failed`. A fourth value still ends
            // the turn, so it closes as completed — but counted and stated,
            // never folded in silently (decision 0031).
            other => {
                let label = if other.is_empty() { "missing" } else { other };
                self.count_unrecognized(&format!("turn/completed/{label}"), params);
                vec![
                    HarnessEvent::HarnessNotice {
                        level: HarnessNoticeLevel::Warning,
                        message: bound(
                            &format!(
                                "the engine ended the turn with an unrecognized status \
                                 ({label}); it was recorded as completed"
                            ),
                            MAX_NOTICE_CHARS,
                        ),
                    },
                    HarnessEvent::TurnCompleted {
                        usage: self.last_usage.clone(),
                    },
                ]
            }
        }
    }

    fn parse_rpc_result(&mut self, value: &Value) -> Vec<HarnessEvent> {
        let id = value.get("id").map(id_key).unwrap_or_default();
        let method = self.outbound_methods.remove(&id).unwrap_or_default();
        let result = value.get("result").cloned().unwrap_or(Value::Null);
        match method.as_str() {
            "thread/start" | "thread/resume" => self.emit_session_started(&result),
            "turn/start" => {
                if let Some(turn_id) = result.pointer("/turn/id").and_then(Value::as_str) {
                    self.last_turn_id = Some(turn_id.to_owned());
                }
                Vec::new()
            }
            "initialize" | "turn/interrupt" | "turn/steer" | "" => Vec::new(),
            other => {
                self.count_unrecognized(&format!("rpc-result/{other}"), value);
                Vec::new()
            }
        }
    }

    fn parse_rpc_error(&mut self, value: &Value) -> Vec<HarnessEvent> {
        let id = value.get("id").map(id_key).unwrap_or_default();
        let method = self.outbound_methods.remove(&id).unwrap_or_default();
        let message = value
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("engine reported an error");
        if method == "initialize" {
            return vec![HarnessEvent::HarnessNotice {
                level: HarnessNoticeLevel::Error,
                message: bound(message, MAX_NOTICE_CHARS),
            }];
        }
        if method == "turn/steer" {
            // The live session routes this response back to the caller that
            // issued the control RPC. A rejected steer is not a failed turn.
            return Vec::new();
        }
        if method.is_empty() {
            // A cancelled or timed-out control can answer after its waiter is
            // gone. An uncorrelated JSON-RPC error is protocol noise, not a
            // failure of the active user turn.
            self.count_unrecognized("rpc-error/unmatched", value);
            return Vec::new();
        }
        vec![HarnessEvent::TurnFailed {
            error: BoundedError {
                message: bound(message, MAX_NOTICE_CHARS),
            },
        }]
    }

    fn emit_session_started(&mut self, result: &Value) -> Vec<HarnessEvent> {
        let thread = result.get("thread").cloned().unwrap_or(Value::Null);
        if let Some(id) = thread
            .get("id")
            .and_then(Value::as_str)
            .or_else(|| thread.get("sessionId").and_then(Value::as_str))
        {
            self.resume_ref = Some(id.to_owned());
        }
        if let Some(version) = thread.get("cliVersion").and_then(Value::as_str) {
            self.version = Some(version.to_owned());
        }
        if self.emitted_session {
            return Vec::new();
        }
        self.emitted_session = true;
        vec![HarnessEvent::SessionStarted {
            harness_kind: HarnessKind::Codex,
            harness_version: self.version.clone().unwrap_or_else(|| "unknown".into()),
            resume_ref: self.resume_ref.clone(),
        }]
    }

    fn emit_tool_started(&mut self, item: &Value) -> Vec<HarnessEvent> {
        let call_id = item
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        if call_id.is_empty() || !self.started_tools.insert(call_id.clone()) {
            return Vec::new();
        }
        vec![HarnessEvent::ToolStarted {
            call_id,
            name: "commandExecution".into(),
            detail: command_detail(item),
        }]
    }

    fn emit_tool_completed(&mut self, item: &Value) -> Vec<HarnessEvent> {
        let call_id = item
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        if call_id.is_empty() {
            return Vec::new();
        }
        let status = item.get("status").and_then(Value::as_str).unwrap_or("");
        let exit_code = item.get("exitCode").and_then(Value::as_i64);
        let outcome = match status {
            "declined" => ToolOutcome::Denied,
            "failed" => ToolOutcome::Failed,
            "completed" if exit_code.unwrap_or(0) != 0 => ToolOutcome::Failed,
            _ => ToolOutcome::Succeeded,
        };
        let preview = item
            .get("aggregatedOutput")
            .and_then(Value::as_str)
            .unwrap_or("");
        // `item/completed` repeats the whole item, command included, so the
        // resolved call can always name its own subject.
        let detail = command_detail(item);
        vec![HarnessEvent::ToolCompleted {
            call_id,
            outcome,
            preview: bound(preview, MAX_PREVIEW_CHARS),
            detail: (detail.specificity() > 0).then_some(detail),
        }]
    }

    fn count_unrecognized(&mut self, label: &str, payload: impl std::fmt::Display) {
        self.unrecognized += 1;
        let mut rendered = payload.to_string();
        crate::text::truncate_on_char_boundary(&mut rendered, MAX_UNRECOGNIZED_LOG);
        tracing::debug!(
            target: "tidebreak_harness::codex",
            unrecognized = self.unrecognized,
            kind = label,
            payload = %rendered,
            "unrecognized engine event"
        );
    }
}

/// Classification for a `commandExecution` item. Both `item/started` and
/// `item/completed` carry the whole item, so both can name the command.
fn command_detail(item: &Value) -> ToolDetail {
    ToolDetail::Command {
        cmd: bound(
            item.get("command").and_then(Value::as_str).unwrap_or(""),
            MAX_EVENT_TEXT_CHARS,
        ),
        cwd: item
            .get("cwd")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
    }
}

fn usage_from(value: Option<&Value>) -> CodeUsage {
    let Some(value) = value else {
        return CodeUsage::default();
    };
    let last = value.get("last").unwrap_or(value);
    CodeUsage {
        input_tokens: last.get("inputTokens").and_then(Value::as_u64).unwrap_or(0),
        output_tokens: last
            .get("outputTokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cache_read_input_tokens: last
            .get("cachedInputTokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cache_creation_input_tokens: last
            .get("cacheWriteInputTokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    }
}

fn id_key(id: &Value) -> String {
    match id {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        other => other.to_string(),
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
{"dir":"out","msg":{"id":1,"method":"thread/start","params":{}}}
{"dir":"in","msg":{"id":1,"result":{"thread":{"id":"abc","cliVersion":"0.147.0"}}}}
{"dir":"in","msg":{"method":"brand_new_shape","params":{"foo":1}}}
{"dir":"in","msg":{"method":"turn/completed","params":{"turn":{"status":"completed"}}}}
"#;
        let out = CodexStreamParser::parse_ndjson(input);
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
    fn an_unknown_turn_status_is_counted_and_stated_before_the_turn_closes() {
        // The turn still has to end — the plausible wrong implementation folds
        // a fourth status into a plain completion and says nothing.
        let input = r#"
{"dir":"in","msg":{"method":"turn/completed","params":{"turn":{"status":"abandoned"}}}}
"#;
        let out = CodexStreamParser::parse_ndjson(input);
        assert_eq!(out.unrecognized, 1);
        assert!(matches!(
            out.events.first(),
            Some(HarnessEvent::HarnessNotice {
                level: HarnessNoticeLevel::Warning,
                ..
            })
        ));
        assert!(matches!(
            out.events.last(),
            Some(HarnessEvent::TurnCompleted { .. })
        ));
    }

    #[test]
    fn a_rejected_steer_does_not_fail_the_active_turn() {
        let input = r#"
{"dir":"out","msg":{"id":7,"method":"turn/steer","params":{"threadId":"thread","expectedTurnId":"turn","input":[{"type":"text","text":"redirect"}]}}}
{"dir":"in","msg":{"id":7,"error":{"code":-32602,"message":"turn is no longer steerable"}}}
{"dir":"in","msg":{"method":"turn/completed","params":{"turn":{"id":"turn","status":"completed"}}}}
"#;
        let out = CodexStreamParser::parse_ndjson(input);
        assert_eq!(out.unrecognized, 0);
        assert_eq!(
            out.events
                .iter()
                .filter(|event| matches!(event, HarnessEvent::TurnFailed { .. }))
                .count(),
            0
        );
        assert!(matches!(
            out.events.last(),
            Some(HarnessEvent::TurnCompleted { .. })
        ));
    }

    #[test]
    fn an_unmatched_rpc_error_does_not_fail_the_active_turn() {
        let input = r#"
{"dir":"in","msg":{"id":7,"error":{"code":-32602,"message":"late response"}}}
{"dir":"in","msg":{"method":"turn/completed","params":{"turn":{"id":"turn","status":"completed"}}}}
"#;
        let out = CodexStreamParser::parse_ndjson(input);
        assert_eq!(out.unrecognized, 1);
        assert!(!out
            .events
            .iter()
            .any(|event| matches!(event, HarnessEvent::TurnFailed { .. })));
        assert!(matches!(
            out.events.last(),
            Some(HarnessEvent::TurnCompleted { .. })
        ));
    }
}
