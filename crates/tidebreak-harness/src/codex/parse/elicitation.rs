//! Codex 0.153.0 MCP tool confirmations share the elicitation channel with
//! arbitrary server forms. Only confirmations tied to an active tool call
//! enter the tool approval policy.

use super::*;
use crate::HarnessError;
use serde_json::json;

#[derive(Debug, Clone, Copy)]
enum ApprovalChannel {
    Command,
    McpTool,
}

#[derive(Debug, Clone)]
pub(crate) struct PendingApproval {
    pub(crate) rpc_id: Value,
    channel: ApprovalChannel,
}

impl PendingApproval {
    pub(super) fn command(rpc_id: Value) -> Self {
        Self {
            rpc_id,
            channel: ApprovalChannel::Command,
        }
    }

    pub(crate) fn response(&self, decision: &ApprovalDecision) -> Result<Value, HarnessError> {
        let accept = match decision {
            ApprovalDecision::Approve => true,
            ApprovalDecision::Deny { .. } => false,
            _ => {
                return Err(HarnessError::DecisionUnsupported(
                    "the codex approval channel takes accept or decline".into(),
                ))
            }
        };
        let token = if accept { "accept" } else { "decline" };
        let result = match self.channel {
            ApprovalChannel::Command => json!({ "decision": token }),
            ApprovalChannel::McpTool => json!({
                "action": token,
                "content": if accept { json!({}) } else { Value::Null },
                "_meta": null,
            }),
        };
        Ok(json!({ "id": self.rpc_id, "result": result }))
    }

    pub(super) fn replayed_decision(&self, value: &Value) -> Option<ApprovalDecision> {
        let field = match self.channel {
            ApprovalChannel::Command => "/result/decision",
            ApprovalChannel::McpTool => "/result/action",
        };
        match (self.channel, value.pointer(field)?.as_str()?) {
            (_, "accept")
            | (
                ApprovalChannel::Command,
                "acceptForSession" | "approved" | "approved_for_session",
            ) => Some(ApprovalDecision::Approve),
            (_, "decline" | "cancel") | (ApprovalChannel::Command, "abort") => {
                Some(ApprovalDecision::Deny { feedback: None })
            }
            _ => None,
        }
    }
}

#[derive(Debug)]
pub(super) struct ActiveMcpCall {
    thread_id: String,
    turn_id: String,
    server: String,
    tool: String,
    arguments: Value,
}

fn string<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key)?.as_str().filter(|value| !value.is_empty())
}

impl CodexStreamParser {
    pub(super) fn observe_mcp_lifecycle(&mut self, method: &str, params: &Value, parent: bool) {
        match method {
            "turn/started" => {
                if parent {
                    self.active_turns.clear();
                    self.active_mcp_calls.clear();
                    self.pending_approvals.clear();
                }
                if let (Some(thread), Some(turn)) = (
                    string(params, "threadId"),
                    params.pointer("/turn/id").and_then(Value::as_str),
                ) {
                    self.active_turns.insert(thread.into(), turn.into());
                }
            }
            "turn/completed" => {
                if parent {
                    self.active_turns.clear();
                    self.active_mcp_calls.clear();
                    self.pending_approvals.clear();
                } else if let Some(thread) = string(params, "threadId") {
                    self.active_turns.remove(thread);
                    self.active_mcp_calls.retain(|id, call| {
                        if call.thread_id == thread {
                            self.pending_approvals.remove(id);
                            false
                        } else {
                            true
                        }
                    });
                }
            }
            "item/started" if self.parent_turn_active => {
                let Some(item) = params.get("item") else {
                    return;
                };
                if string(item, "type") != Some("mcpToolCall")
                    || string(item, "status") != Some("inProgress")
                {
                    return;
                }
                let (Some(id), Some(thread), Some(turn), Some(server), Some(tool), Some(arguments)) = (
                    string(item, "id"),
                    string(params, "threadId"),
                    string(params, "turnId"),
                    string(item, "server"),
                    string(item, "tool"),
                    item.get("arguments"),
                ) else {
                    return;
                };
                if self.active_turns.get(thread).map(String::as_str) != Some(turn) {
                    return;
                }
                self.active_mcp_calls.insert(
                    id.into(),
                    ActiveMcpCall {
                        thread_id: thread.into(),
                        turn_id: turn.into(),
                        server: server.into(),
                        tool: tool.into(),
                        arguments: arguments.clone(),
                    },
                );
            }
            "item/completed" => {
                if let Some(id) = params.pointer("/item/id").and_then(Value::as_str) {
                    self.active_mcp_calls.remove(id);
                    self.pending_approvals.remove(id);
                }
            }
            "serverRequest/resolved" => {
                if let Some(id) = params.get("requestId") {
                    self.pending_approvals
                        .retain(|_, pending| pending.rpc_id != *id);
                }
            }
            _ => {}
        }
    }

    pub(super) fn parse_elicitation(&mut self, value: &Value, params: &Value) -> Vec<HarnessEvent> {
        if let Some((call_id, raw)) = self.mcp_confirmation(params) {
            if let Some(id) = value
                .get("id")
                .filter(|id| id.is_string() || id.is_i64() || id.is_u64())
            {
                if !self.pending_approvals.contains_key(&call_id)
                    && !self
                        .pending_approvals
                        .values()
                        .any(|pending| pending.rpc_id == *id)
                {
                    self.pending_approvals.insert(
                        call_id.clone(),
                        PendingApproval {
                            rpc_id: id.clone(),
                            channel: ApprovalChannel::McpTool,
                        },
                    );
                    return vec![HarnessEvent::ApprovalRequested {
                        harness_ref: HarnessApprovalRef::engine(call_id),
                        kind: Some(tidebreak_core::ApprovalKind::Other {
                            summary: bound(
                                raw["tool_name"].as_str().unwrap_or("MCP tool"),
                                MAX_TOOL_SUMMARY_CHARS,
                            ),
                        }),
                        raw,
                    }];
                }
            }
        }
        // Do not turn forms, URLs, malformed confirmations, or uncorrelated
        // tool requests into consent. Reply even in Allow mode.
        if let Some(id) = value.get("id") {
            self.rejected_elicitations.push(json!({
                "id": id,
                "result": { "action": "decline", "content": null, "_meta": null },
            }));
        }
        vec![HarnessEvent::HarnessNotice {
            level: HarnessNoticeLevel::Warning,
            message: "Tidebreak does not support this MCP form, URL, or unverified tool confirmation. The request cannot proceed.".into(),
        }]
    }

    fn mcp_confirmation(&self, params: &Value) -> Option<(String, Value)> {
        if !self.parent_turn_active
            || string(params, "mode") != Some("form")
            || params
                .pointer("/_meta/codex_approval_kind")
                .and_then(Value::as_str)
                != Some("mcp_tool_call")
            || params.get("requestedSchema")? != &json!({ "type": "object", "properties": {} })
        {
            return None;
        }
        let thread = string(params, "threadId")?;
        let turn = string(params, "turnId")?;
        let server = string(params, "serverName")?;
        let message = string(params, "message")?;
        let arguments = params.pointer("/_meta/tool_params")?;
        if self.active_turns.get(thread).map(String::as_str) != Some(turn) {
            return None;
        }
        let mut matches = self.active_mcp_calls.iter().filter(|(_, call)| {
            call.thread_id == thread
                && call.turn_id == turn
                && call.server == server
                && call.arguments == *arguments
                && message
                    == format!(
                        "Allow the {} MCP server to run tool \"{}\"?",
                        call.server, call.tool
                    )
        });
        let (call_id, call) = matches.next()?;
        if matches.next().is_some() {
            return None;
        }
        let mut raw = params.clone();
        raw["tool_name"] = json!(format!("mcp__{}__{}", call.server, call.tool));
        raw["input"] = call.arguments.clone();
        raw["itemId"] = json!(call_id);
        Some((call_id.clone(), raw))
    }
}

#[cfg(test)]
mod tests;
