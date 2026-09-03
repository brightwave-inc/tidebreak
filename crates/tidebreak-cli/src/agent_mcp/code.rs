//! Code-mode MCP tools. Reads are [`ApprovalClass::ReadOnly`]; mutations are
//! [`ApprovalClass::Sensitive`].

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tidebreak_core::{
    ApprovalClass, ApprovalId, CapLevel, HarnessKind, PermissionMode, RepoId, Result, SessionId,
    Tool, ToolCtx, ToolErrorCategory, ToolOutput, ToolRegistry, ToolSpec, TurnId, WorkspaceId,
};
use tokio::sync::Mutex;

use super::code_follow::{
    decide_and_follow, run_turn, wait_turn, CodeFollowState, CodeTurnResult, DEFAULT_TIMEOUT,
};
use super::{AgentMcp, TurnStatus};

const DEFAULT_TIMEOUT_SECS: u64 = 300;

struct CodeTools {
    state: Arc<AgentMcp>,
    follows: Mutex<HashMap<SessionId, CodeFollowState>>,
}

/// Register every code-mode tool on `registry`.
pub(crate) fn register(registry: &mut ToolRegistry, state: Arc<AgentMcp>) {
    let tools = Arc::new(CodeTools {
        state,
        follows: Mutex::new(HashMap::new()),
    });
    registry.register(Box::new(CodeHarnessesTool {
        tools: Arc::clone(&tools),
    }));
    registry.register(Box::new(CodeRepoAddTool {
        tools: Arc::clone(&tools),
    }));
    registry.register(Box::new(CodeReposTool {
        tools: Arc::clone(&tools),
    }));
    registry.register(Box::new(CodeWorkspaceCreateTool {
        tools: Arc::clone(&tools),
    }));
    registry.register(Box::new(CodeWorkspacesTool {
        tools: Arc::clone(&tools),
    }));
    registry.register(Box::new(CodeWorkspaceArchiveTool {
        tools: Arc::clone(&tools),
    }));
    registry.register(Box::new(CodeSessionCreateTool {
        tools: Arc::clone(&tools),
    }));
    registry.register(Box::new(CodeSessionsTool {
        tools: Arc::clone(&tools),
    }));
    registry.register(Box::new(CodeSessionSetPermissionModeTool {
        tools: Arc::clone(&tools),
    }));
    registry.register(Box::new(CodeRunTurnTool {
        tools: Arc::clone(&tools),
    }));
    registry.register(Box::new(CodeWaitTool {
        tools: Arc::clone(&tools),
    }));
    registry.register(Box::new(CodeApprovalsTool {
        tools: Arc::clone(&tools),
    }));
    registry.register(Box::new(CodeDecideTool {
        tools: Arc::clone(&tools),
    }));
    registry.register(Box::new(CodeInterruptTool {
        tools: Arc::clone(&tools),
    }));
    registry.register(Box::new(CodeTurnsTool {
        tools: Arc::clone(&tools),
    }));
    registry.register(Box::new(CodeDiffTool {
        tools: Arc::clone(&tools),
    }));
    registry.register(Box::new(CodeFilesTool {
        tools: Arc::clone(&tools),
    }));
    registry.register(Box::new(CodeGitStatusTool {
        tools: Arc::clone(&tools),
    }));
    registry.register(Box::new(CodeGitCommitTool {
        tools: Arc::clone(&tools),
    }));
    registry.register(Box::new(CodeGitPushTool {
        tools: Arc::clone(&tools),
    }));
    registry.register(Box::new(CodeGitPrTool { tools }));
}

fn turn_output(result: &CodeTurnResult) -> ToolOutput {
    let data = serde_json::to_value(result).unwrap_or(Value::Null);
    ToolOutput::text(format!(
        "status: {}{}",
        result.status.as_str(),
        if result.assistant_text.is_empty() {
            String::new()
        } else {
            format!("\n{}", result.assistant_text)
        }
    ))
    .with_data(data)
}

fn fail(message: impl Into<String>) -> Result<ToolOutput> {
    Ok(ToolOutput::error(message.into()))
}

fn fail_args(message: impl Into<String>) -> Result<ToolOutput> {
    Ok(ToolOutput::failed(
        ToolErrorCategory::InvalidArguments,
        message.into(),
    ))
}

fn object_schema(properties: Value, required: &[&str]) -> Value {
    let mut schema = json!({
        "type": "object",
        "properties": properties,
        "additionalProperties": false,
    });
    if !required.is_empty() {
        schema["required"] = json!(required);
    }
    schema
}

fn timeout_property() -> Value {
    json!({
        "type": "integer",
        "minimum": 1,
        "default": DEFAULT_TIMEOUT_SECS,
        "description": "Seconds to follow the turn before returning status running. The turn keeps going server-side.",
    })
}

fn permission_mode_property() -> Value {
    json!({
        "type": "string",
        "enum": ["plan", "ask", "auto", "allow"],
        "description": "Permission mode stored on the session.",
    })
}

fn harness_property() -> Value {
    json!({
        "type": "string",
        "enum": ["claude_code", "codex", "opencode", "grok"],
        "description": "Engine adapter kind.",
    })
}

fn timeout_from(args: &Value) -> std::result::Result<Duration, ToolOutput> {
    match args.get("timeout_seconds") {
        None | Some(Value::Null) => Ok(DEFAULT_TIMEOUT),
        Some(value) => {
            let Some(seconds) = value.as_u64() else {
                return Err(ToolOutput::failed(
                    ToolErrorCategory::InvalidArguments,
                    "timeout_seconds must be a positive integer",
                ));
            };
            if seconds == 0 {
                return Err(ToolOutput::failed(
                    ToolErrorCategory::InvalidArguments,
                    "timeout_seconds must be at least 1",
                ));
            }
            Ok(Duration::from_secs(seconds))
        }
    }
}

fn required_uuid<T: FromStr>(args: &Value, field: &str) -> std::result::Result<T, ToolOutput> {
    let Some(value) = args.get(field).and_then(Value::as_str) else {
        return Err(ToolOutput::failed(
            ToolErrorCategory::InvalidArguments,
            format!("{field} is required"),
        ));
    };
    T::from_str(value).map_err(|_| {
        ToolOutput::failed(
            ToolErrorCategory::InvalidArguments,
            format!("{field} must be a UUID"),
        )
    })
}

fn optional_uuid<T: FromStr>(
    args: &Value,
    field: &str,
) -> std::result::Result<Option<T>, ToolOutput> {
    match args.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => {
            let Some(text) = value.as_str() else {
                return Err(ToolOutput::failed(
                    ToolErrorCategory::InvalidArguments,
                    format!("{field} must be a UUID"),
                ));
            };
            T::from_str(text).map(Some).map_err(|_| {
                ToolOutput::failed(
                    ToolErrorCategory::InvalidArguments,
                    format!("{field} must be a UUID"),
                )
            })
        }
    }
}

fn parse_permission_mode(value: &str) -> std::result::Result<PermissionMode, ToolOutput> {
    PermissionMode::from_str(value).ok_or_else(|| {
        ToolOutput::failed(
            ToolErrorCategory::InvalidArguments,
            "permission_mode must be plan, ask, auto, or allow",
        )
    })
}

fn parse_harness(value: &str) -> std::result::Result<HarnessKind, ToolOutput> {
    HarnessKind::from_str(value).ok_or_else(|| {
        ToolOutput::failed(
            ToolErrorCategory::InvalidArguments,
            "harness must be claude_code, codex, opencode, or grok",
        )
    })
}

fn default_create_permission_mode(caps: Option<&tidebreak_core::HarnessCaps>) -> PermissionMode {
    match caps {
        Some(caps) if caps.allow_mode == CapLevel::Supported => PermissionMode::Allow,
        Some(caps) if caps.auto_mode == CapLevel::Supported => PermissionMode::Auto,
        Some(caps) if caps.structured_approvals == CapLevel::Supported => PermissionMode::Ask,
        _ => PermissionMode::Plan,
    }
}

async fn remember(tools: &CodeTools, session: SessionId, result: &CodeTurnResult) {
    let Some(turn_id) = result.turn_id else {
        return;
    };
    tools.follows.lock().await.insert(
        session,
        CodeFollowState {
            turn_id,
            last_seq: result.events_cursor,
            assistant_text: result.assistant_text.clone(),
            queued: result.status == TurnStatus::Queued,
        },
    );
}

fn parse_decision(
    value: &Value,
) -> std::result::Result<(Option<ApprovalId>, bool, Option<String>), ToolOutput> {
    let approval_id = match value.get("approval_id") {
        None | Some(Value::Null) => None,
        Some(id) => {
            let Some(text) = id.as_str() else {
                return Err(ToolOutput::failed(
                    ToolErrorCategory::InvalidArguments,
                    "approval_id must be a UUID",
                ));
            };
            Some(ApprovalId::from_str(text).map_err(|_| {
                ToolOutput::failed(
                    ToolErrorCategory::InvalidArguments,
                    "approval_id must be a UUID",
                )
            })?)
        }
    };
    let approve = if let Some(approve) = value.get("approve") {
        approve.as_bool().ok_or_else(|| {
            ToolOutput::failed(
                ToolErrorCategory::InvalidArguments,
                "approve must be a boolean",
            )
        })?
    } else {
        match value.get("decision").and_then(Value::as_str) {
            Some("approve") => true,
            Some("deny") | Some("reject") => false,
            Some(other) => {
                return Err(ToolOutput::failed(
                    ToolErrorCategory::InvalidArguments,
                    format!("decision must be approve or deny, not {other:?}"),
                ));
            }
            None => {
                return Err(ToolOutput::failed(
                    ToolErrorCategory::InvalidArguments,
                    "decision needs approve or decision",
                ));
            }
        }
    };
    let feedback = value
        .get("feedback")
        .or_else(|| value.get("reason"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    Ok((approval_id, approve, feedback))
}

// ---------------------------------------------------------------------------
// code_harnesses
// ---------------------------------------------------------------------------

struct CodeHarnessesTool {
    tools: Arc<CodeTools>,
}

#[async_trait::async_trait]
impl Tool for CodeHarnessesTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "code_harnesses".into(),
            description:
                "Harness doctor report: which engines are found, their tier, and capabilities."
                    .into(),
            input_schema: object_schema(json!({}), &[]),
        }
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::ReadOnly
    }

    async fn execute(&self, _ctx: &ToolCtx, _args: Value) -> Result<ToolOutput> {
        let client = self.tools.state.client.lock().await;
        match client.list_harnesses().await {
            Ok(report) => Ok(
                ToolOutput::text(format!("{} harness(es)", report.harnesses.len()))
                    .with_data(serde_json::to_value(&report).unwrap_or(Value::Null)),
            ),
            Err(error) => fail(error.to_string()),
        }
    }
}

// ---------------------------------------------------------------------------
// code_repo_add / code_repos
// ---------------------------------------------------------------------------

struct CodeRepoAddTool {
    tools: Arc<CodeTools>,
}

#[async_trait::async_trait]
impl Tool for CodeRepoAddTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "code_repo_add".into(),
            description: "Register a local git repository as a code-mode repo.".into(),
            input_schema: object_schema(
                json!({
                    "source": {
                        "type": "string",
                        "description": "Absolute path to the git checkout.",
                    },
                    "name": {
                        "type": "string",
                        "description": "Display name. Defaults to the directory name.",
                    },
                }),
                &["source"],
            ),
        }
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::Sensitive
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let Some(source) = args.get("source").and_then(Value::as_str) else {
            return fail_args("source is required");
        };
        let name = args.get("name").and_then(Value::as_str);
        let client = self.tools.state.client.lock().await;
        match client.create_repo(source, name, None, None).await {
            Ok(repo) => Ok(ToolOutput::text(format!("repo {}", repo.id))
                .with_data(serde_json::to_value(&repo).unwrap_or(Value::Null))),
            Err(error) => fail(error.to_string()),
        }
    }
}

struct CodeReposTool {
    tools: Arc<CodeTools>,
}

#[async_trait::async_trait]
impl Tool for CodeReposTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "code_repos".into(),
            description: "List registered code-mode repositories.".into(),
            input_schema: object_schema(json!({}), &[]),
        }
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::ReadOnly
    }

    async fn execute(&self, _ctx: &ToolCtx, _args: Value) -> Result<ToolOutput> {
        let client = self.tools.state.client.lock().await;
        match client.list_repos().await {
            Ok(repos) => Ok(ToolOutput::text(format!("{} repo(s)", repos.len()))
                .with_data(json!({ "repos": repos }))),
            Err(error) => fail(error.to_string()),
        }
    }
}

// ---------------------------------------------------------------------------
// workspaces
// ---------------------------------------------------------------------------

struct CodeWorkspaceCreateTool {
    tools: Arc<CodeTools>,
}

#[async_trait::async_trait]
impl Tool for CodeWorkspaceCreateTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "code_workspace_create".into(),
            description: "Create an isolated workspace (worktree + branch) on a repo.".into(),
            input_schema: object_schema(
                json!({
                    "repo_id": {
                        "type": "string",
                        "description": "Repository UUID.",
                    },
                    "name": {
                        "type": "string",
                        "description": "Workspace title.",
                    },
                    "base": {
                        "type": "string",
                        "description": "Base ref. Defaults to the repo's default_base_ref.",
                    },
                }),
                &["repo_id"],
            ),
        }
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::Sensitive
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let repo_id = match required_uuid::<RepoId>(&args, "repo_id") {
            Ok(id) => id,
            Err(output) => return Ok(output),
        };
        let name = args.get("name").and_then(Value::as_str);
        let base = args.get("base").and_then(Value::as_str);
        let client = self.tools.state.client.lock().await;
        match client.create_workspace(repo_id, name, base).await {
            Ok(workspace) => Ok(ToolOutput::text(format!("workspace {}", workspace.id))
                .with_data(serde_json::to_value(&workspace).unwrap_or(Value::Null))),
            Err(error) => fail(error.to_string()),
        }
    }
}

struct CodeWorkspacesTool {
    tools: Arc<CodeTools>,
}

#[async_trait::async_trait]
impl Tool for CodeWorkspacesTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "code_workspaces".into(),
            description: "List code-mode workspaces, optionally filtered by repo.".into(),
            input_schema: object_schema(
                json!({
                    "repo_id": {
                        "type": "string",
                        "description": "Limit the list to this repository UUID.",
                    },
                }),
                &[],
            ),
        }
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::ReadOnly
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let repo_id = match optional_uuid::<RepoId>(&args, "repo_id") {
            Ok(id) => id,
            Err(output) => return Ok(output),
        };
        let client = self.tools.state.client.lock().await;
        match client.list_workspaces(repo_id).await {
            Ok(workspaces) => Ok(
                ToolOutput::text(format!("{} workspace(s)", workspaces.len()))
                    .with_data(json!({ "workspaces": workspaces })),
            ),
            Err(error) => fail(error.to_string()),
        }
    }
}

struct CodeWorkspaceArchiveTool {
    tools: Arc<CodeTools>,
}

#[async_trait::async_trait]
impl Tool for CodeWorkspaceArchiveTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "code_workspace_archive".into(),
            description: "Archive a workspace.".into(),
            input_schema: object_schema(
                json!({
                    "workspace_id": {
                        "type": "string",
                        "description": "Workspace UUID.",
                    },
                }),
                &["workspace_id"],
            ),
        }
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::Sensitive
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let workspace_id = match required_uuid::<WorkspaceId>(&args, "workspace_id") {
            Ok(id) => id,
            Err(output) => return Ok(output),
        };
        let client = self.tools.state.client.lock().await;
        match client.archive_workspace(workspace_id, false).await {
            Ok(workspace) => Ok(ToolOutput::text(format!("archived {}", workspace.id))
                .with_data(serde_json::to_value(&workspace).unwrap_or(Value::Null))),
            Err(error) => fail(error.to_string()),
        }
    }
}

// ---------------------------------------------------------------------------
// sessions
// ---------------------------------------------------------------------------

struct CodeSessionCreateTool {
    tools: Arc<CodeTools>,
}

#[async_trait::async_trait]
impl Tool for CodeSessionCreateTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "code_session_create".into(),
            description: "Start a code-mode session in a workspace.".into(),
            input_schema: object_schema(
                json!({
                    "workspace_id": {
                        "type": "string",
                        "description": "Workspace UUID.",
                    },
                    "harness": harness_property(),
                    "model": {
                        "type": "string",
                        "description": "Engine model id to pin on the session.",
                    },
                    "permission_mode": permission_mode_property(),
                }),
                &["workspace_id"],
            ),
        }
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::Sensitive
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let workspace_id = match required_uuid::<WorkspaceId>(&args, "workspace_id") {
            Ok(id) => id,
            Err(output) => return Ok(output),
        };
        let harness = match args.get("harness").and_then(Value::as_str) {
            Some(value) => match parse_harness(value) {
                Ok(kind) => kind,
                Err(output) => return Ok(output),
            },
            None => HarnessKind::ClaudeCode,
        };
        let model = args.get("model").and_then(Value::as_str);
        let client = self.tools.state.client.lock().await;
        let mode = match args.get("permission_mode").and_then(Value::as_str) {
            Some(value) => match parse_permission_mode(value) {
                Ok(mode) => mode,
                Err(output) => return Ok(output),
            },
            None => {
                let caps = match client.list_harnesses().await {
                    Ok(report) => report
                        .harnesses
                        .into_iter()
                        .find(|entry| entry.kind == harness)
                        .map(|entry| entry.caps),
                    Err(error) => return fail(error.to_string()),
                };
                default_create_permission_mode(caps.as_ref())
            }
        };
        match client
            .create_session_with(workspace_id, harness, mode, model)
            .await
        {
            Ok(session) => Ok(ToolOutput::text(format!("session {}", session.id))
                .with_data(serde_json::to_value(&session).unwrap_or(Value::Null))),
            Err(error) => fail(error.to_string()),
        }
    }
}

struct CodeSessionsTool {
    tools: Arc<CodeTools>,
}

#[async_trait::async_trait]
impl Tool for CodeSessionsTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "code_sessions".into(),
            description: "List sessions in a workspace.".into(),
            input_schema: object_schema(
                json!({
                    "workspace_id": {
                        "type": "string",
                        "description": "Workspace UUID.",
                    },
                }),
                &["workspace_id"],
            ),
        }
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::ReadOnly
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let workspace_id = match required_uuid::<WorkspaceId>(&args, "workspace_id") {
            Ok(id) => id,
            Err(output) => return Ok(output),
        };
        let client = self.tools.state.client.lock().await;
        match client.list_workspace_sessions(workspace_id).await {
            Ok(sessions) => Ok(ToolOutput::text(format!("{} session(s)", sessions.len()))
                .with_data(json!({ "sessions": sessions }))),
            Err(error) => fail(error.to_string()),
        }
    }
}

struct CodeSessionSetPermissionModeTool {
    tools: Arc<CodeTools>,
}

#[async_trait::async_trait]
impl Tool for CodeSessionSetPermissionModeTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "code_session_set_permission_mode".into(),
            description: "Change a session's permission mode.".into(),
            input_schema: object_schema(
                json!({
                    "session_id": {
                        "type": "string",
                        "description": "Session UUID.",
                    },
                    "permission_mode": permission_mode_property(),
                }),
                &["session_id", "permission_mode"],
            ),
        }
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::Sensitive
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let session_id = match required_uuid::<SessionId>(&args, "session_id") {
            Ok(id) => id,
            Err(output) => return Ok(output),
        };
        let Some(mode) = args.get("permission_mode").and_then(Value::as_str) else {
            return fail_args("permission_mode is required");
        };
        let mode = match parse_permission_mode(mode) {
            Ok(mode) => mode,
            Err(output) => return Ok(output),
        };
        let client = self.tools.state.client.lock().await;
        match client.set_session_permission_mode(session_id, mode).await {
            Ok(session) => Ok(ToolOutput::text(format!(
                "session {} is now {}",
                session.id,
                session.permission_mode.as_str()
            ))
            .with_data(serde_json::to_value(&session).unwrap_or(Value::Null))),
            Err(error) => fail(error.to_string()),
        }
    }
}

// ---------------------------------------------------------------------------
// turns / follow
// ---------------------------------------------------------------------------

struct CodeRunTurnTool {
    tools: Arc<CodeTools>,
}

#[async_trait::async_trait]
impl Tool for CodeRunTurnTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "code_run_turn".into(),
            description: "Submit a prompt and follow until the turn settles, parks on an approval, queues, or the timeout elapses."
                .into(),
            input_schema: object_schema(
                json!({
                    "session_id": {
                        "type": "string",
                        "description": "Session UUID.",
                    },
                    "prompt": {
                        "type": "string",
                        "description": "User text to post as this turn.",
                    },
                    "timeout_seconds": timeout_property(),
                }),
                &["session_id", "prompt"],
            ),
        }
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::Sensitive
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let session_id = match required_uuid::<SessionId>(&args, "session_id") {
            Ok(id) => id,
            Err(output) => return Ok(output),
        };
        let Some(prompt) = args.get("prompt").and_then(Value::as_str) else {
            return fail_args("prompt is required");
        };
        let timeout = match timeout_from(&args) {
            Ok(timeout) => timeout,
            Err(output) => return Ok(output),
        };
        let mut client = self.tools.state.client.lock().await;
        let result = match run_turn(&mut client, session_id, prompt, timeout).await {
            Ok(result) => result,
            Err(error) => return fail(error.to_string()),
        };
        drop(client);
        remember(&self.tools, session_id, &result).await;
        Ok(turn_output(&result))
    }
}

struct CodeWaitTool {
    tools: Arc<CodeTools>,
}

#[async_trait::async_trait]
impl Tool for CodeWaitTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "code_wait".into(),
            description: "Re-follow an in-flight or queued turn until it settles, parks, or the timeout elapses."
                .into(),
            input_schema: object_schema(
                json!({
                    "session_id": {
                        "type": "string",
                        "description": "Session UUID.",
                    },
                    "timeout_seconds": timeout_property(),
                }),
                &["session_id"],
            ),
        }
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::ReadOnly
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let session_id = match required_uuid::<SessionId>(&args, "session_id") {
            Ok(id) => id,
            Err(output) => return Ok(output),
        };
        let timeout = match timeout_from(&args) {
            Ok(timeout) => timeout,
            Err(output) => return Ok(output),
        };
        let follow = self.tools.follows.lock().await.get(&session_id).cloned();
        let Some(follow) = follow else {
            return fail(format!("no in-flight turn for session {session_id}"));
        };
        let mut client = self.tools.state.client.lock().await;
        let result = match wait_turn(&mut client, session_id, follow, timeout).await {
            Ok(result) => result,
            Err(error) => return fail(error.to_string()),
        };
        drop(client);
        remember(&self.tools, session_id, &result).await;
        Ok(turn_output(&result))
    }
}

struct CodeApprovalsTool {
    tools: Arc<CodeTools>,
}

#[async_trait::async_trait]
impl Tool for CodeApprovalsTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "code_approvals".into(),
            description: "List pending approvals for a code session.".into(),
            input_schema: object_schema(
                json!({
                    "session_id": {
                        "type": "string",
                        "description": "Session UUID.",
                    },
                }),
                &["session_id"],
            ),
        }
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::ReadOnly
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let session_id = match required_uuid::<SessionId>(&args, "session_id") {
            Ok(id) => id,
            Err(output) => return Ok(output),
        };
        let client = self.tools.state.client.lock().await;
        match client.list_approvals(Some(session_id), true).await {
            Ok(approvals) => Ok(ToolOutput::text(format!("{} approval(s)", approvals.len()))
                .with_data(json!({ "approvals": approvals }))),
            Err(error) => fail(error.to_string()),
        }
    }
}

struct CodeDecideTool {
    tools: Arc<CodeTools>,
}

#[async_trait::async_trait]
impl Tool for CodeDecideTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "code_decide".into(),
            description:
                "Approve or deny a parked code-mode approval, then follow to the next settle point."
                    .into(),
            input_schema: object_schema(
                json!({
                    "session_id": {
                        "type": "string",
                        "description": "Session UUID.",
                    },
                    "decision": {
                        "type": "object",
                        "description": "Approval decision: {approval_id?, decision:\"approve\"|\"deny\", feedback?} or {approve: true}.",
                    },
                    "timeout_seconds": timeout_property(),
                }),
                &["session_id", "decision"],
            ),
        }
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::Sensitive
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let session_id = match required_uuid::<SessionId>(&args, "session_id") {
            Ok(id) => id,
            Err(output) => return Ok(output),
        };
        let Some(decision_value) = args.get("decision") else {
            return fail_args("decision is required");
        };
        let (approval_id, approve, feedback) = match parse_decision(decision_value) {
            Ok(parsed) => parsed,
            Err(output) => return Ok(output),
        };
        let timeout = match timeout_from(&args) {
            Ok(timeout) => timeout,
            Err(output) => return Ok(output),
        };
        let follow = self.tools.follows.lock().await.get(&session_id).cloned();
        let mut client = self.tools.state.client.lock().await;
        let approval_id = match approval_id {
            Some(id) => id,
            None => {
                let pending = match client.list_approvals(Some(session_id), true).await {
                    Ok(pending) => pending,
                    Err(error) => return fail(error.to_string()),
                };
                match pending.as_slice() {
                    [only] => only.id,
                    [] => return fail(format!("no pending approval on session {session_id}")),
                    _ => return fail_args(
                        "decision must include approval_id when more than one approval is pending",
                    ),
                }
            }
        };
        let result = match decide_and_follow(
            &mut client,
            session_id,
            approval_id,
            approve,
            feedback.as_deref(),
            follow,
            timeout,
        )
        .await
        {
            Ok(result) => result,
            Err(error) => return fail(error.to_string()),
        };
        drop(client);
        remember(&self.tools, session_id, &result).await;
        Ok(turn_output(&result))
    }
}

struct CodeInterruptTool {
    tools: Arc<CodeTools>,
}

#[async_trait::async_trait]
impl Tool for CodeInterruptTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "code_interrupt".into(),
            description: "Interrupt the in-flight turn on a session.".into(),
            input_schema: object_schema(
                json!({
                    "session_id": {
                        "type": "string",
                        "description": "Session UUID.",
                    },
                }),
                &["session_id"],
            ),
        }
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::Sensitive
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let session_id = match required_uuid::<SessionId>(&args, "session_id") {
            Ok(id) => id,
            Err(output) => return Ok(output),
        };
        let client = self.tools.state.client.lock().await;
        if let Err(error) = client.interrupt_session(session_id).await {
            return fail(error.to_string());
        }
        Ok(ToolOutput::text("interrupted").with_data(json!({ "ok": true })))
    }
}

struct CodeTurnsTool {
    tools: Arc<CodeTools>,
}

#[async_trait::async_trait]
impl Tool for CodeTurnsTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "code_turns".into(),
            description: "Turn history and the durable session queue.".into(),
            input_schema: object_schema(
                json!({
                    "session_id": {
                        "type": "string",
                        "description": "Session UUID.",
                    },
                }),
                &["session_id"],
            ),
        }
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::ReadOnly
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let session_id = match required_uuid::<SessionId>(&args, "session_id") {
            Ok(id) => id,
            Err(output) => return Ok(output),
        };
        let client = self.tools.state.client.lock().await;
        let turns = match client.list_session_turns(session_id).await {
            Ok(turns) => turns,
            Err(error) => return fail(error.to_string()),
        };
        let queued = match client.list_queued_code_turns(session_id).await {
            Ok(queued) => queued,
            Err(error) => return fail(error.to_string()),
        };
        let data = json!({
            "turns": turns,
            "queued": queued.queued,
            "queue_paused": queued.paused,
        });
        Ok(ToolOutput::text(format!(
            "{} turn(s), {} queued",
            turns.len(),
            queued.queued.len()
        ))
        .with_data(data))
    }
}

// ---------------------------------------------------------------------------
// diff / files / git
// ---------------------------------------------------------------------------

struct CodeDiffTool {
    tools: Arc<CodeTools>,
}

#[async_trait::async_trait]
impl Tool for CodeDiffTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "code_diff".into(),
            description: "Bounded unified diff for a workspace, optionally one path.".into(),
            input_schema: object_schema(
                json!({
                    "workspace_id": {
                        "type": "string",
                        "description": "Workspace UUID.",
                    },
                    "path": {
                        "type": "string",
                        "description": "Limit the diff to this worktree-relative path.",
                    },
                }),
                &["workspace_id"],
            ),
        }
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::ReadOnly
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let workspace_id = match required_uuid::<WorkspaceId>(&args, "workspace_id") {
            Ok(id) => id,
            Err(output) => return Ok(output),
        };
        let path = args.get("path").and_then(Value::as_str);
        let client = self.tools.state.client.lock().await;
        match client
            .workspace_diff(workspace_id, None::<TurnId>, path)
            .await
        {
            Ok(diff) => Ok(ToolOutput::text(if diff.diff.is_empty() {
                "no diff".into()
            } else {
                diff.diff.clone()
            })
            .with_data(serde_json::to_value(&diff).unwrap_or(Value::Null))),
            Err(error) => fail(error.to_string()),
        }
    }
}

struct CodeFilesTool {
    tools: Arc<CodeTools>,
}

#[async_trait::async_trait]
impl Tool for CodeFilesTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "code_files".into(),
            description: "Changed-file list for a workspace, optionally filtered by path.".into(),
            input_schema: object_schema(
                json!({
                    "workspace_id": {
                        "type": "string",
                        "description": "Workspace UUID.",
                    },
                    "path": {
                        "type": "string",
                        "description": "Keep files whose path equals or is under this prefix.",
                    },
                }),
                &["workspace_id"],
            ),
        }
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::ReadOnly
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let workspace_id = match required_uuid::<WorkspaceId>(&args, "workspace_id") {
            Ok(id) => id,
            Err(output) => return Ok(output),
        };
        let path = args.get("path").and_then(Value::as_str);
        let client = self.tools.state.client.lock().await;
        let mut files = match client.workspace_files(workspace_id, None).await {
            Ok(files) => files,
            Err(error) => return fail(error.to_string()),
        };
        if let Some(path) = path {
            files
                .files
                .retain(|file| file.path == path || file.path.starts_with(&format!("{path}/")));
        }
        Ok(ToolOutput::text(format!("{} file(s)", files.files.len()))
            .with_data(serde_json::to_value(&files).unwrap_or(Value::Null)))
    }
}

struct CodeGitStatusTool {
    tools: Arc<CodeTools>,
}

#[async_trait::async_trait]
impl Tool for CodeGitStatusTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "code_git_status".into(),
            description: "Local git status and pull-request digest for a workspace.".into(),
            input_schema: object_schema(
                json!({
                    "workspace_id": {
                        "type": "string",
                        "description": "Workspace UUID.",
                    },
                }),
                &["workspace_id"],
            ),
        }
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::ReadOnly
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let workspace_id = match required_uuid::<WorkspaceId>(&args, "workspace_id") {
            Ok(id) => id,
            Err(output) => return Ok(output),
        };
        let client = self.tools.state.client.lock().await;
        match client.git_status(workspace_id).await {
            Ok(status) => {
                let summary = if status.dirty { "dirty" } else { "clean" };
                Ok(ToolOutput::text(format!("git {summary}"))
                    .with_data(serde_json::to_value(&status).unwrap_or(Value::Null)))
            }
            Err(error) => fail(error.to_string()),
        }
    }
}

struct CodeGitCommitTool {
    tools: Arc<CodeTools>,
}

#[async_trait::async_trait]
impl Tool for CodeGitCommitTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "code_git_commit".into(),
            description: "Stage and commit the workspace worktree.".into(),
            input_schema: object_schema(
                json!({
                    "workspace_id": {
                        "type": "string",
                        "description": "Workspace UUID.",
                    },
                    "message": {
                        "type": "string",
                        "description": "Commit message.",
                    },
                }),
                &["workspace_id", "message"],
            ),
        }
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::Sensitive
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let workspace_id = match required_uuid::<WorkspaceId>(&args, "workspace_id") {
            Ok(id) => id,
            Err(output) => return Ok(output),
        };
        let Some(message) = args.get("message").and_then(Value::as_str) else {
            return fail_args("message is required");
        };
        let client = self.tools.state.client.lock().await;
        match client.git_commit(workspace_id, Some(message)).await {
            Ok(commit) => Ok(ToolOutput::text(format!("committed {}", commit.sha))
                .with_data(serde_json::to_value(&commit).unwrap_or(Value::Null))),
            Err(error) => fail(error.to_string()),
        }
    }
}

struct CodeGitPushTool {
    tools: Arc<CodeTools>,
}

#[async_trait::async_trait]
impl Tool for CodeGitPushTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "code_git_push".into(),
            description: "Push the workspace branch.".into(),
            input_schema: object_schema(
                json!({
                    "workspace_id": {
                        "type": "string",
                        "description": "Workspace UUID.",
                    },
                }),
                &["workspace_id"],
            ),
        }
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::Sensitive
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let workspace_id = match required_uuid::<WorkspaceId>(&args, "workspace_id") {
            Ok(id) => id,
            Err(output) => return Ok(output),
        };
        let client = self.tools.state.client.lock().await;
        match client.git_push(workspace_id).await {
            Ok(push) => Ok(
                ToolOutput::text(format!("pushed {} to {}", push.branch, push.remote))
                    .with_data(serde_json::to_value(&push).unwrap_or(Value::Null)),
            ),
            Err(error) => fail(error.to_string()),
        }
    }
}

struct CodeGitPrTool {
    tools: Arc<CodeTools>,
}

#[async_trait::async_trait]
impl Tool for CodeGitPrTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "code_git_pr".into(),
            description: "Open a pull request for the workspace branch.".into(),
            input_schema: object_schema(
                json!({
                    "workspace_id": {
                        "type": "string",
                        "description": "Workspace UUID.",
                    },
                    "title": {
                        "type": "string",
                        "description": "Pull request title.",
                    },
                    "body": {
                        "type": "string",
                        "description": "Pull request body.",
                    },
                    "draft": {
                        "type": "boolean",
                        "description": "Ignored: the git_pr route has no draft field.",
                    },
                }),
                &["workspace_id"],
            ),
        }
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::Sensitive
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let workspace_id = match required_uuid::<WorkspaceId>(&args, "workspace_id") {
            Ok(id) => id,
            Err(output) => return Ok(output),
        };
        let title = args.get("title").and_then(Value::as_str);
        let body = args.get("body").and_then(Value::as_str);
        let client = self.tools.state.client.lock().await;
        match client.git_pr(workspace_id, title, body).await {
            Ok(pr) => Ok(ToolOutput::text("pull request")
                .with_data(serde_json::to_value(&pr).unwrap_or(Value::Null))),
            Err(error) => fail(error.to_string()),
        }
    }
}
