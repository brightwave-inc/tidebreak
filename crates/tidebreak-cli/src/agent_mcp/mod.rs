//! `tidebreak agent-mcp` — a stdio MCP server that drives chat over attach.
//!
//! Possession of the server bearer is the authorization boundary. The registry
//! is wired with [`AutoApproveGate`] so every tool is listed and callable; the
//! *driven chat's* own approvals, plans, and questions are never auto-approved.
//! Those park as interaction points and come back as `needs_approval` /
//! `needs_plan_decision` / `needs_answer` for `chat_decide` to answer.
//!
//! Host folder consent is not drivable. A turn that parks on
//! `request_folder_access` returns `needs_host_consent` with no decision path.
//! Standing consent comes from `tidebreak folder connect` or the desktop.
//!
//! Later surfaces add tool modules by adding a file and one
//! [`register`] call in [`assemble_registry`].

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tidebreak_core::{
    AgentError, AutoApproveGate, Result, SessionId, ToolCtx, ToolRegistry, TurnId,
};
use tokio::sync::Mutex;

use crate::api::client::Client;
use crate::connect;

mod chat;
mod code;
mod code_follow;
pub(crate) mod follow;
mod profile;

/// Per-chat follow cursor so `chat_wait` / `chat_decide` resume the same turn.
#[derive(Debug, Clone)]
struct FollowState {
    turn_id: TurnId,
    last_seq: i64,
    assistant_text: String,
}

/// Shared process state for every mounted tool.
struct AgentMcp {
    client: Mutex<Client>,
    follows: Mutex<HashMap<SessionId, FollowState>>,
}

/// Serve `tidebreak agent-mcp`: resolve the attach (or embed) endpoint, build
/// the chat tool registry, and run MCP over stdio. stdout is JSON-RPC only.
pub(crate) async fn run(server: connect::Server) -> Result<()> {
    let session = connect::Session::open(&server).await?;
    let state = Arc::new(AgentMcp {
        client: Mutex::new(session.client().clone()),
        follows: Mutex::new(HashMap::new()),
    });

    let tools = Arc::new(assemble_registry(state));
    let ctx = ToolCtx::without_private_scratch(SessionId::new(), None);
    let mcp =
        tidebreak_mcp::McpServer::new(tools, ctx).with_approval_gate(Arc::new(AutoApproveGate));

    let outcome = tidebreak_mcp::serve_stdio(mcp)
        .await
        .map_err(|error| AgentError::msg(format!("MCP stdio error: {error}")));
    // Keep the embedded accept loop alive until stdio ends; dropping `session`
    // earlier would abort an in-process server mid-call.
    drop(session);
    outcome
}

/// Assemble the MCP registry. Add a module and one `register` call here to
/// grow the surface without touching the entry point.
fn assemble_registry(state: Arc<AgentMcp>) -> ToolRegistry {
    let mut tools = ToolRegistry::new();
    chat::register(&mut tools, Arc::clone(&state));
    profile::register(&mut tools, Arc::clone(&state));
    code::register(&mut tools, state);
    tools
}

/// The run-turn / wait / decide return contract.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TurnResult {
    pub status: TurnStatus,
    pub assistant_text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending: Option<serde_json::Value>,
    pub events_cursor: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TurnStatus {
    Completed,
    NeedsApproval,
    NeedsPlanDecision,
    NeedsAnswer,
    NeedsHostConsent,
    Running,
    Queued,
    Cancelled,
    Failed,
}

impl TurnStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::NeedsApproval => "needs_approval",
            Self::NeedsPlanDecision => "needs_plan_decision",
            Self::NeedsAnswer => "needs_answer",
            Self::NeedsHostConsent => "needs_host_consent",
            Self::Running => "running",
            Self::Queued => "queued",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_registered_tool_schema_is_valid_json_schema() {
        let client = Client::attach("http://127.0.0.1:9".into(), "token").expect("client");
        let registry = assemble_registry(Arc::new(AgentMcp {
            client: Mutex::new(client),
            follows: Mutex::new(HashMap::new()),
        }));
        let specs = registry.specs();
        let names: Vec<&str> = specs.iter().map(|spec| spec.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "agent_run_cancel",
                "agent_runs",
                "chat_attach_file",
                "chat_cancel",
                "chat_create",
                "chat_decide",
                "chat_events",
                "chat_list",
                "chat_output_read",
                "chat_outputs",
                "chat_run_turn",
                "chat_set_model",
                "chat_set_permission_mode",
                "chat_status",
                "chat_steer",
                "chat_wait",
                "code_approvals",
                "code_decide",
                "code_diff",
                "code_files",
                "code_git_commit",
                "code_git_pr",
                "code_git_push",
                "code_git_status",
                "code_harnesses",
                "code_interrupt",
                "code_repo_add",
                "code_repos",
                "code_run_turn",
                "code_session_create",
                "code_session_set_permission_mode",
                "code_sessions",
                "code_turns",
                "code_wait",
                "code_workspace_archive",
                "code_workspace_create",
                "code_workspaces",
                "exec_select",
                "model_role_set",
                "profile_snapshot",
                "web_search_select",
            ],
            "an invalid schema is dropped from advertisement; names must stay complete"
        );
        for spec in &specs {
            jsonschema::options()
                .with_draft(jsonschema::Draft::Draft202012)
                .build(&spec.input_schema)
                .unwrap_or_else(|error| {
                    panic!(
                        "{} input schema is not valid JSON Schema: {error}\n{}",
                        spec.name, spec.input_schema
                    )
                });
        }
    }
}
