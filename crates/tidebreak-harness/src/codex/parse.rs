//! Parse captured Codex app-server JSON-RPC frames into [`HarnessEvent`]s.
//!
//! Written against the checked-in fixtures under `fixtures/codex/0.147.0/`
//! and that version's generated app-server schema. Unknown methods increment
//! a counter and are logged (size-capped). They are never fatal and never
//! dropped silently.
//!
//! Fixture lines are framed `{ "dir": "in"|"out", "msg": <json-rpc> }`. A
//! bare JSON-RPC object is treated as inbound.

use std::collections::{HashMap, HashSet};

use serde_json::Value;
use tidebreak_core::{
    BoundedError, CodeUsage, HarnessKind, HarnessNoticeLevel, ToolDetail, ToolOutcome,
    MAX_EVENT_TEXT_CHARS, MAX_NOTICE_CHARS, MAX_PREVIEW_CHARS, MAX_TOOL_SUMMARY_CHARS,
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
    /// Codex child thread ids whose synthetic `Task` span has started.
    started_subagents: HashSet<String>,
    /// Child thread ids whose synthetic `Task` span has settled.
    settled_subagents: HashSet<String>,
    /// Best display detail observed for each child thread.
    subagent_details: HashMap<String, ToolDetail>,
    /// The containing `Task` for nested child threads. Top-level children map
    /// to `None` because their spawn ran on the parent thread.
    subagent_parents: HashMap<String, Option<String>>,
    emitted_session: bool,
    /// Child frames are accepted only while their parent turn is open.
    parent_turn_active: bool,
    /// Thread-wide counters at the start of the active turn.
    turn_usage_baseline: CodeUsage,
    /// Prompt resident on the first usage update after `turn/started`.
    turn_first_call_context_tokens: Option<u64>,
    /// Latest thread-wide counters plus the final call's context occupancy.
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
                harness_ref: HarnessApprovalRef::engine(call_id),
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
        let parent_thread = self.is_parent_thread(&params);
        if parent_thread {
            if let Some(turn_id) = params
                .get("turnId")
                .and_then(Value::as_str)
                .or_else(|| params.pointer("/turn/id").and_then(Value::as_str))
            {
                self.last_turn_id = Some(turn_id.to_owned());
            }
        }
        match method {
            "item/agentMessage/delta" => {
                // A completed child message is journaled with its `Task` id.
                // The transient delta has no attribution field, so emitting it
                // would flatten the child's text into the parent transcript.
                if !parent_thread {
                    return Vec::new();
                }
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
            "turn/started" => {
                if !parent_thread {
                    return self.ensure_subagent_started(&params);
                }
                self.parent_turn_active = true;
                self.turn_usage_baseline = self.last_usage.clone();
                self.turn_first_call_context_tokens = None;
                vec![HarnessEvent::TurnStarted]
            }
            "turn/completed" if parent_thread => self.parse_turn_completed(&params),
            "turn/completed" => self.parse_subagent_turn_completed(&params),
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
                // The app server multiplexes child threads over the parent's
                // connection. Their counters must not replace the parent's.
                if parent_thread {
                    let usage = usage_from(params.get("tokenUsage"));
                    if self.parent_turn_active && self.turn_first_call_context_tokens.is_none() {
                        self.turn_first_call_context_tokens =
                            (usage.context_tokens > 0).then_some(usage.context_tokens);
                    }
                    self.last_usage = usage;
                }
                Vec::new()
            }
            "account/rateLimits/updated"
            | "hook/completed"
            | "hook/started"
            | "item/commandExecution/outputDelta"
            | "item/fileChange/outputDelta"
            | "item/fileChange/patchUpdated"
            | "item/mcpToolCall/progress"
            | "item/plan/delta"
            | "item/reasoning/summaryPartAdded"
            | "item/reasoning/summaryTextDelta"
            | "item/reasoning/textDelta"
            | "mcpServer/startupStatus/updated"
            | "remoteControl/status/changed"
            | "serverRequest/resolved"
            | "thread/compacted"
            | "thread/goal/updated"
            | "thread/queue/changed"
            | "thread/settings/updated"
            | "thread/started"
            | "thread/status/changed"
            | "thread/goal/cleared"
            | "turn/diff/updated"
            | "turn/plan/updated" => {
                // Known app-server state and streaming notifications that do
                // not change the normalized transcript. Treating them as
                // unknown makes long, healthy turns look like protocol drift.
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
        let parent_call_id = self.parent_call_id(params);
        if parent_call_id.is_some() && !self.parent_turn_active {
            return Vec::new();
        }
        match item.get("type").and_then(Value::as_str) {
            Some("commandExecution") => self.emit_tool_started(&item, parent_call_id),
            Some("fileChange") => self.emit_file_change_started(&item, parent_call_id),
            Some("collabAgentToolCall") => self.emit_collab_started(&item, parent_call_id),
            Some("subAgentActivity" | "userMessage" | "agentMessage" | "reasoning") => Vec::new(),
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
        let parent_call_id = self.parent_call_id(params);
        if parent_call_id.is_some() && !self.parent_turn_active {
            return Vec::new();
        }
        match item.get("type").and_then(Value::as_str) {
            Some("commandExecution") => self.emit_tool_completed(&item, parent_call_id),
            Some("fileChange") => self.emit_file_change_completed(&item, parent_call_id),
            Some("collabAgentToolCall") => self.emit_collab_completed(&item, parent_call_id),
            Some("agentMessage") => {
                let text = item.get("text").and_then(Value::as_str).unwrap_or("");
                if text.is_empty() {
                    Vec::new()
                } else {
                    vec![HarnessEvent::AssistantMessage {
                        text: bound(text, MAX_EVENT_TEXT_CHARS),
                        parent_call_id,
                    }]
                }
            }
            Some("subAgentActivity" | "userMessage" | "reasoning") => Vec::new(),
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
            harness_ref: HarnessApprovalRef::engine(call_id),
            raw: params.clone(),
        }]
    }

    fn parse_turn_completed(&mut self, params: &Value) -> Vec<HarnessEvent> {
        let status = params
            .pointer("/turn/status")
            .and_then(Value::as_str)
            .unwrap_or("");
        // Parent lifecycle is the outer bound for every child (decision 52).
        // A late child frame must not reopen a span that this boundary settles.
        self.settled_subagents
            .extend(self.started_subagents.iter().cloned());
        self.parent_turn_active = false;
        match status {
            "completed" => vec![HarnessEvent::TurnCompleted {
                usage: turn_usage_since(
                    &self.last_usage,
                    &self.turn_usage_baseline,
                    self.turn_first_call_context_tokens,
                ),
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
            // `completed | interrupted | failed`. Anything else is protocol
            // drift and must fail closed.
            other => {
                let label = if other.is_empty() { "missing" } else { other };
                self.count_unrecognized(&format!("turn/completed/{label}"), params);
                vec![
                    HarnessEvent::HarnessNotice {
                        level: HarnessNoticeLevel::Warning,
                        message: bound(
                            &format!(
                                "the engine ended the turn with an unrecognized status \
                                 ({label}); it was recorded as failed"
                            ),
                            MAX_NOTICE_CHARS,
                        ),
                    },
                    HarnessEvent::TurnFailed {
                        error: BoundedError {
                            message: bound(
                                &format!("Codex protocol drift: turn ended with status {label}"),
                                MAX_NOTICE_CHARS,
                            ),
                        },
                    },
                ]
            }
        }
    }

    /// Whether a multiplexed app-server notification belongs to the thread
    /// Tidebreak attached. Notifications without a thread id keep the legacy
    /// parent behavior.
    fn is_parent_thread(&self, params: &Value) -> bool {
        let Some(thread_id) = thread_id(params) else {
            return true;
        };
        self.resume_ref
            .as_deref()
            .is_none_or(|parent| thread_id == parent)
    }

    /// The synthetic `Task` span for a child-thread notification.
    fn parent_call_id(&self, params: &Value) -> Option<String> {
        (!self.is_parent_thread(params))
            .then(|| thread_id(params).map(str::to_owned))
            .flatten()
    }

    fn ensure_subagent_started(&mut self, params: &Value) -> Vec<HarnessEvent> {
        if !self.parent_turn_active {
            return Vec::new();
        }
        let Some(thread_id) = self.parent_call_id(params) else {
            return Vec::new();
        };
        let parent = self
            .subagent_parents
            .get(&thread_id)
            .cloned()
            .unwrap_or(None);
        let detail = self
            .subagent_details
            .get(&thread_id)
            .cloned()
            .unwrap_or_else(generic_subagent_detail);
        self.emit_subagent_started(thread_id, parent, detail)
    }

    fn parse_subagent_turn_completed(&mut self, params: &Value) -> Vec<HarnessEvent> {
        if !self.parent_turn_active {
            return Vec::new();
        }
        let Some(thread_id) = self.parent_call_id(params) else {
            return Vec::new();
        };
        let status = params
            .pointer("/turn/status")
            .and_then(Value::as_str)
            .unwrap_or("");
        let (outcome, preview) = match status {
            "completed" => (ToolOutcome::Succeeded, String::new()),
            "interrupted" => (ToolOutcome::Failed, "Subagent interrupted".to_owned()),
            "failed" => (
                ToolOutcome::Failed,
                params
                    .pointer("/turn/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("Subagent failed")
                    .to_owned(),
            ),
            other => {
                let label = if other.is_empty() { "missing" } else { other };
                self.count_unrecognized(&format!("subagent/turn-completed/{label}"), params);
                (
                    ToolOutcome::Failed,
                    format!("Subagent ended with an unrecognized status ({label})"),
                )
            }
        };
        self.settle_subagent(thread_id, outcome, preview)
    }

    fn emit_collab_started(
        &mut self,
        item: &Value,
        parent_call_id: Option<String>,
    ) -> Vec<HarnessEvent> {
        match item.get("tool").and_then(Value::as_str) {
            Some("spawnAgent") => self.start_spawned_subagents(item, parent_call_id),
            Some("sendInput" | "resumeAgent" | "wait" | "closeAgent") => {
                self.start_collab_companions(item)
            }
            Some(other) => {
                self.count_unrecognized(&format!("collab/tool/{other}"), item);
                Vec::new()
            }
            None => {
                self.count_unrecognized("collab/tool/missing", item);
                Vec::new()
            }
        }
    }

    fn emit_collab_completed(
        &mut self,
        item: &Value,
        parent_call_id: Option<String>,
    ) -> Vec<HarnessEvent> {
        match item.get("tool").and_then(Value::as_str) {
            Some("spawnAgent") => {
                let targets = collab_targets(item);
                let outcome = self.collab_outcome(item);
                let mut events = self.start_spawned_subagents(item, parent_call_id.clone());
                if targets.is_empty() {
                    if outcome != ToolOutcome::Succeeded {
                        let call_id = item
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned();
                        if !call_id.is_empty() {
                            let detail = subagent_detail(item);
                            events.extend(self.emit_subagent_started(
                                call_id.clone(),
                                parent_call_id,
                                detail,
                            ));
                            events.extend(self.settle_subagent(
                                call_id,
                                outcome,
                                "Codex could not start the subagent".to_owned(),
                            ));
                        }
                    }
                    return events;
                }
                for target in targets {
                    if outcome != ToolOutcome::Succeeded {
                        events.extend(self.settle_subagent(
                            target,
                            outcome,
                            "Codex could not start the subagent".to_owned(),
                        ));
                    } else if let Some((task_outcome, preview)) =
                        subagent_state_outcome(item, &target)
                    {
                        events.extend(self.settle_subagent(target, task_outcome, preview));
                    }
                }
                events
            }
            Some("sendInput" | "resumeAgent" | "wait" | "closeAgent") => {
                let targets = collab_targets(item);
                let mut events = self.start_collab_companions(item);
                let outcome = self.collab_outcome(item);
                for target in targets {
                    let call_id = collab_call_id(item, &target);
                    if !call_id.is_empty() {
                        events.push(HarnessEvent::ToolCompleted {
                            call_id,
                            outcome,
                            preview: bound(&collab_preview(item, &target), MAX_PREVIEW_CHARS),
                            detail: None,
                            parent_call_id: Some(target.clone()),
                        });
                    }
                    if let Some((task_outcome, preview)) = subagent_state_outcome(item, &target) {
                        events.extend(self.settle_subagent(target, task_outcome, preview));
                    }
                }
                events
            }
            Some(other) => {
                self.count_unrecognized(&format!("collab/tool/{other}"), item);
                Vec::new()
            }
            None => {
                self.count_unrecognized("collab/tool/missing", item);
                Vec::new()
            }
        }
    }

    fn start_spawned_subagents(
        &mut self,
        item: &Value,
        parent_call_id: Option<String>,
    ) -> Vec<HarnessEvent> {
        let detail = subagent_detail(item);
        let mut events = Vec::new();
        for target in collab_targets(item) {
            events.extend(self.emit_subagent_started(
                target,
                parent_call_id.clone(),
                detail.clone(),
            ));
        }
        events
    }

    fn emit_subagent_started(
        &mut self,
        thread_id: String,
        parent_call_id: Option<String>,
        detail: ToolDetail,
    ) -> Vec<HarnessEvent> {
        self.subagent_details
            .insert(thread_id.clone(), detail.clone());
        self.subagent_parents
            .insert(thread_id.clone(), parent_call_id.clone());
        if !self.started_subagents.insert(thread_id.clone()) {
            return Vec::new();
        }
        vec![HarnessEvent::ToolStarted {
            call_id: thread_id,
            name: "Task".into(),
            detail,
            parent_call_id,
        }]
    }

    fn start_collab_companions(&mut self, item: &Value) -> Vec<HarnessEvent> {
        let mut events = Vec::new();
        for target in collab_targets(item) {
            let call_id = collab_call_id(item, &target);
            if call_id.is_empty() || !self.started_tools.insert(call_id.clone()) {
                continue;
            }
            events.push(HarnessEvent::ToolStarted {
                call_id,
                name: collab_tool_name(item).to_owned(),
                detail: collab_detail(item, &target),
                parent_call_id: Some(target),
            });
        }
        events
    }

    fn settle_subagent(
        &mut self,
        thread_id: String,
        outcome: ToolOutcome,
        preview: String,
    ) -> Vec<HarnessEvent> {
        let parent_call_id = self
            .subagent_parents
            .get(&thread_id)
            .cloned()
            .unwrap_or(None);
        let detail = self
            .subagent_details
            .get(&thread_id)
            .cloned()
            .unwrap_or_else(generic_subagent_detail);
        let mut events =
            self.emit_subagent_started(thread_id.clone(), parent_call_id.clone(), detail.clone());
        if !self.settled_subagents.insert(thread_id.clone()) {
            return events;
        }
        events.push(HarnessEvent::ToolCompleted {
            call_id: thread_id,
            outcome,
            preview: bound(&preview, MAX_PREVIEW_CHARS),
            detail: Some(detail),
            parent_call_id,
        });
        events
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
                    return Vec::new();
                }
                self.count_unrecognized("rpc-result/turn-start/missing-turn-id", value);
                vec![HarnessEvent::TurnFailed {
                    error: BoundedError {
                        message: "Codex returned a malformed turn/start result".into(),
                    },
                }]
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
        if matches!(method.as_str(), "turn/steer" | "turn/interrupt") {
            // The live session routes control responses back to their callers.
            // A rejected control request is not itself a failed turn.
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

    fn emit_tool_started(
        &mut self,
        item: &Value,
        parent_call_id: Option<String>,
    ) -> Vec<HarnessEvent> {
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
            parent_call_id,
        }]
    }

    fn emit_tool_completed(
        &mut self,
        item: &Value,
        parent_call_id: Option<String>,
    ) -> Vec<HarnessEvent> {
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
            "failed" | "cancelled" | "canceled" => ToolOutcome::Failed,
            "completed" if exit_code.unwrap_or(0) != 0 => ToolOutcome::Failed,
            "completed" => ToolOutcome::Succeeded,
            other => {
                let label = if other.is_empty() { "missing" } else { other };
                self.count_unrecognized(&format!("commandExecution/completed/{label}"), item);
                ToolOutcome::Failed
            }
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
            parent_call_id,
        }]
    }

    fn emit_file_change_started(
        &mut self,
        item: &Value,
        parent_call_id: Option<String>,
    ) -> Vec<HarnessEvent> {
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
            name: "fileChange".into(),
            detail: file_change_detail(item),
            parent_call_id,
        }]
    }

    fn emit_file_change_completed(
        &mut self,
        item: &Value,
        parent_call_id: Option<String>,
    ) -> Vec<HarnessEvent> {
        let call_id = item
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        if call_id.is_empty() {
            return Vec::new();
        }
        let detail = file_change_detail(item);
        let mut events = Vec::new();
        if self.started_tools.insert(call_id.clone()) {
            events.push(HarnessEvent::ToolStarted {
                call_id: call_id.clone(),
                name: "fileChange".into(),
                detail: detail.clone(),
                parent_call_id: parent_call_id.clone(),
            });
        }
        let status = item.get("status").and_then(Value::as_str).unwrap_or("");
        let outcome = match status {
            "declined" => ToolOutcome::Denied,
            "failed" | "cancelled" | "canceled" => ToolOutcome::Failed,
            "completed" => ToolOutcome::Succeeded,
            other => {
                let label = if other.is_empty() { "missing" } else { other };
                self.count_unrecognized(&format!("fileChange/completed/{label}"), item);
                ToolOutcome::Failed
            }
        };
        events.push(HarnessEvent::ToolCompleted {
            call_id,
            outcome,
            preview: file_change_preview(item),
            detail: (detail.specificity() > 0).then_some(detail),
            parent_call_id,
        });
        events
    }

    fn collab_outcome(&mut self, item: &Value) -> ToolOutcome {
        match item.get("status").and_then(Value::as_str) {
            Some("completed") => ToolOutcome::Succeeded,
            Some("declined") => ToolOutcome::Denied,
            Some("failed" | "interrupted" | "cancelled" | "canceled") => ToolOutcome::Failed,
            Some(other) => {
                self.count_unrecognized(&format!("collab/completed/{other}"), item);
                ToolOutcome::Failed
            }
            None => {
                self.count_unrecognized("collab/completed/missing", item);
                ToolOutcome::Failed
            }
        }
    }

    fn count_unrecognized(&mut self, label: &str, payload: impl std::fmt::Display) {
        self.unrecognized += 1;
        let mut rendered = payload.to_string();
        crate::text::truncate_on_char_boundary(&mut rendered, MAX_UNRECOGNIZED_LOG);
        // The kind alone is what makes a dropped event findable later, and
        // it carries no engine payload, so it rides at info. The truncated
        // body stays at debug for whoever is actually chasing one.
        tracing::info!(
            target: "tidebreak_harness::codex",
            unrecognized = self.unrecognized,
            kind = label,
            "unrecognized engine event"
        );
        tracing::debug!(
            target: "tidebreak_harness::codex",
            kind = label,
            payload = %rendered,
            "unrecognized engine event payload"
        );
    }
}

fn thread_id(params: &Value) -> Option<&str> {
    params.get("threadId").and_then(Value::as_str)
}

fn collab_targets(item: &Value) -> Vec<String> {
    item.get("receiverThreadIds")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|thread_id| !thread_id.is_empty())
        .map(str::to_owned)
        .collect()
}

fn collab_tool_name(item: &Value) -> &str {
    match item.get("tool").and_then(Value::as_str) {
        Some("sendInput") => "SendInput",
        Some("resumeAgent") => "ResumeAgent",
        Some("wait") => "WaitAgent",
        Some("closeAgent") => "CloseAgent",
        _ => "Subagent",
    }
}

fn collab_call_id(item: &Value, target: &str) -> String {
    let call_id = item.get("id").and_then(Value::as_str).unwrap_or("");
    if call_id.is_empty() || target.is_empty() {
        String::new()
    } else {
        format!("{call_id}:{target}")
    }
}

fn first_nonempty_line(value: Option<&str>) -> Option<&str> {
    value?.lines().map(str::trim).find(|line| !line.is_empty())
}

fn generic_subagent_detail() -> ToolDetail {
    ToolDetail::Other {
        summary: "Subagent".to_owned(),
    }
}

fn subagent_detail(item: &Value) -> ToolDetail {
    let summary =
        first_nonempty_line(item.get("prompt").and_then(Value::as_str)).unwrap_or("Subagent");
    ToolDetail::Other {
        summary: bound(summary, MAX_TOOL_SUMMARY_CHARS),
    }
}

fn collab_detail(item: &Value, target: &str) -> ToolDetail {
    let target = &target[..8.min(target.len())];
    let summary = match item.get("tool").and_then(Value::as_str) {
        Some("sendInput") => first_nonempty_line(item.get("prompt").and_then(Value::as_str))
            .map(|prompt| format!("Send to {target}: {prompt}"))
            .unwrap_or_else(|| format!("Send input to {target}")),
        Some("resumeAgent") => format!("Resume subagent {target}"),
        Some("wait") => format!("Wait for subagent {target}"),
        Some("closeAgent") => format!("Close subagent {target}"),
        _ => format!("Subagent {target}"),
    };
    ToolDetail::Other {
        summary: bound(&summary, MAX_TOOL_SUMMARY_CHARS),
    }
}

fn collab_preview(item: &Value, target: &str) -> String {
    item.pointer(&format!("/agentsStates/{target}/message"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}

fn subagent_state_outcome(item: &Value, target: &str) -> Option<(ToolOutcome, String)> {
    let state = item.pointer(&format!("/agentsStates/{target}"))?;
    let status = state.get("status").and_then(Value::as_str)?;
    let preview = state
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    match status {
        "completed" => Some((ToolOutcome::Succeeded, preview)),
        "interrupted" | "errored" | "shutdown" | "notFound" => Some((ToolOutcome::Failed, preview)),
        "pendingInit" | "running" => None,
        _ => None,
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

fn file_change_detail(item: &Value) -> ToolDetail {
    let path = item
        .get("changes")
        .and_then(Value::as_array)
        .and_then(|changes| changes.first())
        .and_then(|change| change.get("path"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    ToolDetail::FileEdit { path }
}

fn file_change_preview(item: &Value) -> String {
    let preview = item
        .get("changes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|change| change.get("diff").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    bound(&preview, MAX_PREVIEW_CHARS)
}

/// Normalize `thread/tokenUsage` into disjoint cumulative counters.
///
/// Two corrections over reading the payload verbatim.
///
/// The engine reports `total` (the thread so far) beside `last` (the most
/// recent model call). A thread that makes several calls has genuinely
/// different values in the two — `total.totalTokens 28912` against
/// `last.totalTokens 14471` in `approval-approve.ndjson`. The parser snapshots
/// these counters at `turn/started` and subtracts them at completion.
///
/// `inputTokens` is the whole prompt: `totalTokens == inputTokens +
/// outputTokens` holds in every captured payload, and `cachedInputTokens` is
/// a subset of it. Filing that subset into `cache_read_input_tokens` while
/// leaving it inside `input_tokens` double-counts it. Subtract the cached and
/// written portions so the four fields stay disjoint and still sum to the
/// prompt.
fn usage_from(value: Option<&Value>) -> CodeUsage {
    let Some(value) = value else {
        return CodeUsage::default();
    };
    let total = value.get("total").unwrap_or(value);
    let field = |name: &str| total.get(name).and_then(Value::as_u64).unwrap_or(0);
    let cache_read_input_tokens = field("cachedInputTokens");
    let cache_creation_input_tokens = field("cacheWriteInputTokens");
    CodeUsage {
        input_tokens: field("inputTokens")
            .saturating_sub(cache_read_input_tokens)
            .saturating_sub(cache_creation_input_tokens),
        output_tokens: field("outputTokens"),
        cache_read_input_tokens,
        cache_creation_input_tokens,
        context_tokens: context_tokens_from(value),
        first_call_context_tokens: None,
    }
}

/// Convert Codex's thread-wide counters into the current turn's usage.
///
/// A resumed thread publishes its previous cumulative total immediately
/// before `turn/started`. Subtracting that snapshot keeps later turns from
/// inheriting every earlier turn's spend. If the engine resets a counter,
/// treat the latest value as a fresh total instead of subtracting past zero.
fn turn_usage_since(
    total: &CodeUsage,
    baseline: &CodeUsage,
    first_call_context_tokens: Option<u64>,
) -> CodeUsage {
    let counters_reset = total.input_tokens < baseline.input_tokens
        || total.output_tokens < baseline.output_tokens
        || total.cache_read_input_tokens < baseline.cache_read_input_tokens
        || total.cache_creation_input_tokens < baseline.cache_creation_input_tokens;
    let baseline = if counters_reset {
        CodeUsage::default()
    } else {
        baseline.clone()
    };
    CodeUsage {
        input_tokens: total.input_tokens.saturating_sub(baseline.input_tokens),
        output_tokens: total.output_tokens.saturating_sub(baseline.output_tokens),
        cache_read_input_tokens: total
            .cache_read_input_tokens
            .saturating_sub(baseline.cache_read_input_tokens),
        cache_creation_input_tokens: total
            .cache_creation_input_tokens
            .saturating_sub(baseline.cache_creation_input_tokens),
        context_tokens: total.context_tokens,
        first_call_context_tokens,
    }
}

/// The prompt resident on the turn's last model call.
///
/// `last` is that call, and its `inputTokens` is the whole prompt it sent.
/// Both cache figures are subsets already inside that number, so neither is
/// added back.
///
/// That `cacheWriteInputTokens` sits inside rather than beside `inputTokens`
/// is what the payload's own arithmetic says: `totalTokens` equals
/// `inputTokens + outputTokens` in every captured object, so `inputTokens`
/// has to carry the whole prompt or the total would not balance once a write
/// is non-zero. [`usage_from`] reads it the same way — it subtracts both cache
/// figures back out to keep the four spend counts disjoint, which is also why
/// this reads the raw payload instead of that remainder.
///
/// Every captured fixture has `cacheWriteInputTokens: 0`, so no test
/// distinguishes the two readings. Adding the write on top would double-count
/// it exactly on a cache miss — the case the context ring exists to show.
///
/// A payload with no `last` is itself one call.
fn context_tokens_from(usage: &Value) -> u64 {
    let last = usage.get("last").unwrap_or(usage);
    last.get("inputTokens").and_then(Value::as_u64).unwrap_or(0)
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
    /// `thread/tokenUsage` folds the cached portion into `inputTokens`.
    /// Reading it verbatim double-counts the cache.
    #[test]
    fn token_usage_is_a_disjoint_thread_total() {
        let payload = serde_json::json!({
            "total": {
                "totalTokens": 28912, "inputTokens": 28794,
                "cachedInputTokens": 23040, "cacheWriteInputTokens": 0,
                "outputTokens": 118, "reasoningOutputTokens": 9
            },
            "last": {
                "totalTokens": 14471, "inputTokens": 14466,
                "cachedInputTokens": 14080, "cacheWriteInputTokens": 0,
                "outputTokens": 5, "reasoningOutputTokens": 0
            }
        });
        let usage = usage_from(Some(&payload));

        // The thread total, not the last call: `last` would report 5.
        assert_eq!(usage.output_tokens, 118);
        // Disjoint: the three prompt-side counts sum to the prompt the engine
        // actually sent, and the cached portion is not also in `input_tokens`.
        assert_eq!(usage.cache_read_input_tokens, 23040);
        assert_eq!(usage.input_tokens, 5754);
        assert_eq!(
            usage.input_tokens + usage.cache_read_input_tokens + usage.cache_creation_input_tokens,
            28794,
            "the split must reconstruct inputTokens"
        );
    }

    #[test]
    fn a_resumed_turn_excludes_the_previous_thread_usage() {
        let baseline = CodeUsage {
            input_tokens: 4_547,
            output_tokens: 6,
            cache_read_input_tokens: 9_984,
            cache_creation_input_tokens: 0,
            context_tokens: 14_531,
            first_call_context_tokens: None,
        };
        let total = CodeUsage {
            input_tokens: 5_819,
            output_tokens: 12,
            cache_read_input_tokens: 24_064,
            cache_creation_input_tokens: 0,
            context_tokens: 15_352,
            first_call_context_tokens: None,
        };

        assert_eq!(
            turn_usage_since(&total, &baseline, Some(15_352)),
            CodeUsage {
                input_tokens: 1_272,
                output_tokens: 6,
                cache_read_input_tokens: 14_080,
                cache_creation_input_tokens: 0,
                context_tokens: 15_352,
                first_call_context_tokens: Some(15_352),
            }
        );
    }

    /// A payload with no `total` (an older shape, or one already narrowed)
    /// still normalizes rather than reporting zeros.
    #[test]
    fn token_usage_without_a_total_falls_back_to_the_object_itself() {
        let payload = serde_json::json!({
            "inputTokens": 100, "cachedInputTokens": 30,
            "cacheWriteInputTokens": 10, "outputTokens": 7
        });
        let usage = usage_from(Some(&payload));
        assert_eq!(usage.input_tokens, 60);
        assert_eq!(usage.cache_read_input_tokens, 30);
        assert_eq!(usage.cache_creation_input_tokens, 10);
        assert_eq!(usage.output_tokens, 7);
    }

    /// A written cache block is inside `inputTokens`, not beside it.
    ///
    /// No captured fixture proves this — every one reports
    /// `cacheWriteInputTokens: 0`. The payload's arithmetic does:
    /// `totalTokens` is `inputTokens + outputTokens`, so `inputTokens` carries
    /// the whole prompt. [`usage_from`] agrees, subtracting the written block
    /// back out to keep the spend counts disjoint.
    ///
    /// Adding it on top instead would inflate the context reading by exactly
    /// the written block on a cache miss — the moment the ring matters most.
    #[test]
    fn a_written_cache_block_is_not_added_on_top_of_the_prompt() {
        let payload = serde_json::json!({
            "total": {
                "totalTokens": 60_020, "inputTokens": 60_000,
                "cachedInputTokens": 12_000, "cacheWriteInputTokens": 8_000,
                "outputTokens": 20, "reasoningOutputTokens": 0
            },
            "last": {
                "totalTokens": 40_007, "inputTokens": 40_000,
                "cachedInputTokens": 12_000, "cacheWriteInputTokens": 8_000,
                "outputTokens": 7, "reasoningOutputTokens": 0
            }
        });

        assert_eq!(
            context_tokens_from(&payload),
            40_000,
            "the resident prompt is the last call's inputTokens, not that plus the write"
        );

        // The same reading `usage_from` takes: the three prompt-side spend
        // counts reconstruct the turn's `inputTokens` exactly.
        let usage = usage_from(Some(&payload));
        assert_eq!(
            usage.input_tokens + usage.cache_read_input_tokens + usage.cache_creation_input_tokens,
            60_000
        );
        // And the resident prompt is below the turn's prompt-side spend,
        // which is the whole reason the two numbers are separate fields.
        assert!(usage.context_tokens < 60_000);
    }

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
    fn known_streaming_telemetry_does_not_count_as_protocol_drift() {
        let input = r#"
{"method":"item/reasoning/summaryTextDelta","params":{"delta":"thinking"}}
{"method":"item/commandExecution/outputDelta","params":{"delta":"output"}}
{"method":"turn/diff/updated","params":{"diff":"patch"}}
{"method":"thread/compacted","params":{}}
"#;
        let out = CodexStreamParser::parse_ndjson(input);

        assert_eq!(out.unrecognized, 0);
        assert!(out.events.is_empty());
    }

    #[test]
    fn file_change_items_emit_a_matched_bounded_edit_span() {
        let long_diff = format!("+{}", "x".repeat(MAX_PREVIEW_CHARS + 20));
        let started = serde_json::json!({
            "method": "item/started",
            "params": {
                "item": {
                    "type": "fileChange",
                    "id": "edit-1",
                    "changes": [{"path": "docs/exercise.md"}]
                }
            }
        });
        let completed = serde_json::json!({
            "method": "item/completed",
            "params": {
                "item": {
                    "type": "fileChange",
                    "id": "edit-1",
                    "status": "completed",
                    "changes": [{"path": "docs/exercise.md", "diff": long_diff}]
                }
            }
        });
        let mut parser = CodexStreamParser::new();
        let mut events = parser.push_line(&started.to_string());
        events.extend(parser.push_line(&completed.to_string()));

        assert_eq!(parser.unrecognized(), 0);
        assert!(matches!(
            events.first(),
            Some(HarnessEvent::ToolStarted {
                call_id,
                detail: ToolDetail::FileEdit { path },
                ..
            }) if call_id == "edit-1" && path == "docs/exercise.md"
        ));
        assert!(matches!(
            events.last(),
            Some(HarnessEvent::ToolCompleted {
                call_id,
                outcome: ToolOutcome::Succeeded,
                preview,
                detail: Some(ToolDetail::FileEdit { path }),
                ..
            }) if call_id == "edit-1"
                && path == "docs/exercise.md"
                && preview.chars().count() == MAX_PREVIEW_CHARS
        ));
    }

    #[test]
    fn first_usage_update_records_starting_context() {
        let input = r#"
{"method":"turn/started","params":{"turn":{"status":"inProgress"}}}
{"method":"thread/tokenUsage/updated","params":{"tokenUsage":{"total":{"inputTokens":15000,"cachedInputTokens":5000,"outputTokens":1},"last":{"inputTokens":15000}}}}
{"method":"thread/tokenUsage/updated","params":{"tokenUsage":{"total":{"inputTokens":31000,"cachedInputTokens":13000,"outputTokens":2},"last":{"inputTokens":16000}}}}
{"method":"turn/completed","params":{"turn":{"status":"completed"}}}
"#;
        let out = CodexStreamParser::parse_ndjson(input);
        let usage = out.events.iter().find_map(|event| match event {
            HarnessEvent::TurnCompleted { usage } => Some(usage),
            _ => None,
        });
        assert_eq!(
            usage.and_then(|usage| usage.first_call_context_tokens),
            Some(15_000)
        );
        assert_eq!(usage.map(|usage| usage.context_tokens), Some(16_000));
    }

    #[test]
    fn an_unknown_turn_status_is_counted_and_stated_before_the_turn_closes() {
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
            Some(HarnessEvent::TurnFailed { .. })
        ));
    }

    #[test]
    fn a_missing_turn_status_fails_closed() {
        let out =
            CodexStreamParser::parse_ndjson(r#"{"method":"turn/completed","params":{"turn":{}}}"#);

        assert_eq!(out.unrecognized, 1);
        assert!(matches!(
            out.events.last(),
            Some(HarnessEvent::TurnFailed { .. })
        ));
    }

    #[test]
    fn unknown_and_missing_command_statuses_fail_closed() {
        for status in [Some("abandoned"), None] {
            let mut item = serde_json::json!({
                "type": "commandExecution",
                "id": "command-1",
                "command": "true",
                "cwd": "/workspace",
                "exitCode": 0
            });
            if let Some(status) = status {
                item["status"] = serde_json::json!(status);
            }
            let input = serde_json::json!({
                "method": "item/completed",
                "params": { "item": item }
            });
            let out = CodexStreamParser::parse_ndjson(&input.to_string());

            assert_eq!(out.unrecognized, 1, "status: {status:?}");
            assert!(out.events.iter().any(|event| matches!(
                event,
                HarnessEvent::ToolCompleted {
                    outcome: ToolOutcome::Failed,
                    ..
                }
            )));
        }
    }

    #[test]
    fn unknown_and_missing_file_change_statuses_fail_closed() {
        for status in [Some("abandoned"), None] {
            let mut item = serde_json::json!({
                "type": "fileChange",
                "id": "edit-1",
                "changes": [{"path": "note.txt", "diff": "+text"}]
            });
            if let Some(status) = status {
                item["status"] = serde_json::json!(status);
            }
            let input = serde_json::json!({
                "method": "item/completed",
                "params": { "item": item }
            });
            let out = CodexStreamParser::parse_ndjson(&input.to_string());

            assert_eq!(out.unrecognized, 1, "status: {status:?}");
            assert!(out.events.iter().any(|event| matches!(
                event,
                HarnessEvent::ToolCompleted {
                    outcome: ToolOutcome::Failed,
                    ..
                }
            )));
        }
    }

    #[test]
    fn unknown_and_missing_collaboration_statuses_fail_closed() {
        for status in [Some("abandoned"), None] {
            let mut item = serde_json::json!({
                "type": "collabAgentToolCall",
                "id": "wait-1",
                "tool": "wait",
                "receiverThreadIds": ["child"],
                "agentsStates": {"child": {"status": "running", "message": null}}
            });
            if let Some(status) = status {
                item["status"] = serde_json::json!(status);
            }
            let input = serde_json::json!({
                "method": "item/completed",
                "params": { "item": item }
            });
            let out = CodexStreamParser::parse_ndjson(&input.to_string());

            assert_eq!(out.unrecognized, 1, "status: {status:?}");
            assert!(out.events.iter().any(|event| matches!(
                event,
                HarnessEvent::ToolCompleted {
                    outcome: ToolOutcome::Failed,
                    ..
                }
            )));
        }
    }

    #[test]
    fn unknown_spawn_status_overrides_a_successful_child_snapshot() {
        let input = r#"{"method":"item/completed","params":{"item":{"type":"collabAgentToolCall","id":"spawn-1","tool":"spawnAgent","status":"abandoned","receiverThreadIds":["child"],"agentsStates":{"child":{"status":"completed","message":"done"}}}}}"#;
        let out = CodexStreamParser::parse_ndjson(input);

        assert_eq!(out.unrecognized, 1);
        assert!(out.events.iter().any(|event| matches!(
            event,
            HarnessEvent::ToolCompleted {
                call_id,
                outcome: ToolOutcome::Failed,
                ..
            } if call_id == "child"
        )));
        assert!(!out.events.iter().any(|event| matches!(
            event,
            HarnessEvent::ToolCompleted {
                call_id,
                outcome: ToolOutcome::Succeeded,
                ..
            } if call_id == "child"
        )));
    }

    #[test]
    fn captured_success_statuses_still_succeed() {
        let input = r#"
{"method":"item/completed","params":{"item":{"type":"commandExecution","id":"command-1","status":"completed","command":"true","cwd":"/workspace","exitCode":0}}}
{"method":"item/completed","params":{"item":{"type":"fileChange","id":"edit-1","status":"completed","changes":[{"path":"note.txt","diff":"+text"}]}}}
{"method":"item/completed","params":{"item":{"type":"collabAgentToolCall","id":"wait-1","tool":"wait","status":"completed","receiverThreadIds":["child"],"agentsStates":{"child":{"status":"running","message":null}}}}}
"#;
        let out = CodexStreamParser::parse_ndjson(input);

        assert_eq!(out.unrecognized, 0);
        assert_eq!(
            out.events
                .iter()
                .filter(|event| matches!(
                    event,
                    HarnessEvent::ToolCompleted {
                        outcome: ToolOutcome::Succeeded,
                        ..
                    }
                ))
                .count(),
            3
        );
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

    #[test]
    fn collaboration_threads_become_task_spans_instead_of_parent_activity() {
        let input = r#"
{"dir":"out","msg":{"id":1,"method":"thread/start","params":{}}}
{"dir":"in","msg":{"id":1,"result":{"thread":{"id":"parent","cliVersion":"0.147.0"}}}}
{"method":"turn/started","params":{"threadId":"parent","turn":{"id":"parent-turn","status":"inProgress"}}}
{"method":"item/started","params":{"threadId":"parent","turnId":"parent-turn","item":{"type":"collabAgentToolCall","id":"spawn-1","tool":"spawnAgent","status":"inProgress","senderThreadId":"parent","receiverThreadIds":[],"prompt":"Inspect the parser.\nReport the result.","model":null,"reasoningEffort":null,"agentsStates":{}}}}
{"method":"item/completed","params":{"threadId":"parent","turnId":"parent-turn","item":{"type":"collabAgentToolCall","id":"spawn-1","tool":"spawnAgent","status":"completed","senderThreadId":"parent","receiverThreadIds":["child"],"prompt":"Inspect the parser.\nReport the result.","model":null,"reasoningEffort":null,"agentsStates":{"child":{"status":"running","message":null}}}}}
{"method":"item/started","params":{"threadId":"parent","turnId":"parent-turn","item":{"type":"subAgentActivity","id":"activity-1","agentThreadId":"child","agentPath":"worker","kind":"started"}}}
{"method":"item/completed","params":{"threadId":"parent","turnId":"parent-turn","item":{"type":"subAgentActivity","id":"activity-1","agentThreadId":"child","agentPath":"worker","kind":"started"}}}
{"method":"turn/started","params":{"threadId":"child","turn":{"id":"child-turn","status":"inProgress"}}}
{"method":"item/started","params":{"threadId":"child","turnId":"child-turn","item":{"type":"commandExecution","id":"child-command","command":"rg parser","cwd":"/workspace","status":"inProgress"}}}
{"method":"item/completed","params":{"threadId":"child","turnId":"child-turn","item":{"type":"commandExecution","id":"child-command","command":"rg parser","cwd":"/workspace","status":"completed","aggregatedOutput":"match\n","exitCode":0}}}
{"method":"item/agentMessage/delta","params":{"threadId":"child","turnId":"child-turn","itemId":"child-message","delta":"Found it"}}
{"method":"item/completed","params":{"threadId":"child","turnId":"child-turn","item":{"type":"agentMessage","id":"child-message","text":"Found it","phase":"final_answer"}}}
{"method":"thread/tokenUsage/updated","params":{"threadId":"child","turnId":"child-turn","tokenUsage":{"total":{"inputTokens":9000,"cachedInputTokens":0,"cacheWriteInputTokens":0,"outputTokens":9000},"last":{"inputTokens":9000}}}}
{"method":"turn/completed","params":{"threadId":"child","turn":{"id":"child-turn","status":"completed"}}}
{"method":"thread/tokenUsage/updated","params":{"threadId":"parent","turnId":"parent-turn","tokenUsage":{"total":{"inputTokens":100,"cachedInputTokens":20,"cacheWriteInputTokens":0,"outputTokens":5},"last":{"inputTokens":70}}}}
{"method":"turn/completed","params":{"threadId":"parent","turn":{"id":"parent-turn","status":"completed"}}}
{"method":"item/started","params":{"threadId":"child","turnId":"child-turn","item":{"type":"commandExecution","id":"late-command","command":"touch late","cwd":"/workspace","status":"inProgress"}}}
{"method":"item/completed","params":{"threadId":"child","turnId":"child-turn","item":{"type":"commandExecution","id":"late-command","command":"touch late","cwd":"/workspace","status":"completed","aggregatedOutput":"","exitCode":0}}}
"#;
        let mut parser = CodexStreamParser::new();
        let mut events = Vec::new();
        for line in input.lines() {
            events.extend(parser.push_line(line));
        }

        assert_eq!(parser.unrecognized(), 0);
        assert_eq!(parser.last_turn_id(), Some("parent-turn"));
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, HarnessEvent::TurnStarted))
                .count(),
            1,
            "a child turn must not start another Tidebreak turn"
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, HarnessEvent::TurnCompleted { .. }))
                .count(),
            1,
            "a child turn must not complete the parent turn"
        );
        assert!(events.iter().any(|event| matches!(
            event,
            HarnessEvent::ToolStarted {
                call_id,
                name,
                detail,
                parent_call_id: None,
            } if call_id == "child" && name == "Task" && detail.subject() == "Inspect the parser."
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            HarnessEvent::ToolStarted {
                call_id,
                parent_call_id: Some(parent),
                ..
            } if call_id == "child-command" && parent == "child"
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            HarnessEvent::AssistantMessage {
                text,
                parent_call_id: Some(parent),
            } if text == "Found it" && parent == "child"
        )));
        assert!(!events.iter().any(
            |event| matches!(event, HarnessEvent::AssistantDelta { text } if text == "Found it")
        ));
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    HarnessEvent::ToolCompleted {
                        call_id,
                        parent_call_id: None,
                        ..
                    } if call_id == "child"
                ))
                .count(),
            1
        );
        let usage = events.iter().find_map(|event| match event {
            HarnessEvent::TurnCompleted { usage } => Some(usage),
            _ => None,
        });
        assert_eq!(usage.map(|usage| usage.output_tokens), Some(5));
        assert_eq!(usage.map(|usage| usage.context_tokens), Some(70));
        assert!(!events.iter().any(|event| matches!(
            event,
            HarnessEvent::ToolStarted { call_id, .. } if call_id == "late-command"
        )));
    }

    #[test]
    fn a_wait_result_settles_each_target_once() {
        let input = r#"
{"dir":"out","msg":{"id":1,"method":"thread/start","params":{}}}
{"dir":"in","msg":{"id":1,"result":{"thread":{"id":"parent","cliVersion":"0.147.0"}}}}
{"method":"turn/started","params":{"threadId":"parent","turn":{"id":"parent-turn","status":"inProgress"}}}
{"method":"item/completed","params":{"threadId":"parent","turnId":"parent-turn","item":{"type":"collabAgentToolCall","id":"spawn-1","tool":"spawnAgent","status":"completed","senderThreadId":"parent","receiverThreadIds":["child"],"prompt":"Run focused checks","model":null,"reasoningEffort":null,"agentsStates":{"child":{"status":"running","message":null}}}}}
{"method":"item/started","params":{"threadId":"parent","turnId":"parent-turn","item":{"type":"collabAgentToolCall","id":"wait-1","tool":"wait","status":"inProgress","senderThreadId":"parent","receiverThreadIds":["child"],"prompt":null,"model":null,"reasoningEffort":null,"agentsStates":{"child":{"status":"running","message":null}}}}}
{"method":"item/completed","params":{"threadId":"parent","turnId":"parent-turn","item":{"type":"collabAgentToolCall","id":"wait-1","tool":"wait","status":"completed","senderThreadId":"parent","receiverThreadIds":["child"],"prompt":null,"model":null,"reasoningEffort":null,"agentsStates":{"child":{"status":"completed","message":"Focused checks passed."}}}}}
{"method":"turn/completed","params":{"threadId":"child","turn":{"id":"child-turn","status":"completed"}}}
{"method":"turn/completed","params":{"threadId":"parent","turn":{"id":"parent-turn","status":"completed"}}}
"#;
        let out = CodexStreamParser::parse_ndjson(input);

        assert_eq!(out.unrecognized, 0);
        assert!(out.events.iter().any(|event| matches!(
            event,
            HarnessEvent::ToolStarted {
                call_id,
                name,
                parent_call_id: Some(parent),
                ..
            } if call_id == "wait-1:child" && name == "WaitAgent" && parent == "child"
        )));
        assert_eq!(
            out.events
                .iter()
                .filter(|event| matches!(
                    event,
                    HarnessEvent::ToolCompleted {
                        call_id,
                        outcome: ToolOutcome::Succeeded,
                        parent_call_id: None,
                        ..
                    } if call_id == "child"
                ))
                .count(),
            1,
            "the later child turn completion must not settle the Task twice"
        );
        assert!(out.events.iter().any(|event| matches!(
            event,
            HarnessEvent::ToolCompleted {
                call_id,
                preview,
                parent_call_id: None,
                ..
            } if call_id == "child" && preview == "Focused checks passed."
        )));
    }
}
