//! Profile and settings MCP tools. Reads are [`ApprovalClass::ReadOnly`];
//! mutations are [`ApprovalClass::Sensitive`].

use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use serde_json::{json, Value};
use tidebreak_core::{
    media_type_is_text, AgentRunId, ApprovalClass, OutputId, OutputRevisionId, Result, SessionId,
    Tool, ToolCtx, ToolErrorCategory, ToolOutput, ToolRegistry, ToolSpec,
};

use super::AgentMcp;

const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp"];
const MODEL_ROLES: &[&str] = &["chat", "utility"];
const PERMISSION_MODES: &[&str] = &["plan", "ask", "auto", "allow"];

/// Register every profile and settings tool on `registry`.
pub(crate) fn register(registry: &mut ToolRegistry, state: Arc<AgentMcp>) {
    registry.register(Box::new(ProfileSnapshotTool {
        state: Arc::clone(&state),
    }));
    registry.register(Box::new(ModelRoleSetTool {
        state: Arc::clone(&state),
    }));
    registry.register(Box::new(WebSearchSelectTool {
        state: Arc::clone(&state),
    }));
    registry.register(Box::new(ExecSelectTool {
        state: Arc::clone(&state),
    }));
    registry.register(Box::new(ChatSetModelTool {
        state: Arc::clone(&state),
    }));
    registry.register(Box::new(ChatSetPermissionModeTool {
        state: Arc::clone(&state),
    }));
    registry.register(Box::new(ChatAttachFileTool {
        state: Arc::clone(&state),
    }));
    registry.register(Box::new(ChatOutputsTool {
        state: Arc::clone(&state),
    }));
    registry.register(Box::new(ChatOutputReadTool {
        state: Arc::clone(&state),
    }));
    registry.register(Box::new(AgentRunsTool {
        state: Arc::clone(&state),
    }));
    registry.register(Box::new(AgentRunCancelTool { state }));
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

fn required_chat_id(args: &Value) -> std::result::Result<SessionId, ToolOutput> {
    let Some(value) = args.get("chat_id").and_then(Value::as_str) else {
        return Err(ToolOutput::failed(
            ToolErrorCategory::InvalidArguments,
            "chat_id is required",
        ));
    };
    SessionId::from_str(value).map_err(|_| {
        ToolOutput::failed(
            ToolErrorCategory::InvalidArguments,
            "chat_id must be a UUID",
        )
    })
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

fn chat_id_property() -> Value {
    json!({
        "type": "string",
        "description": "Chat UUID.",
    })
}

fn optional_provider(args: &Value) -> std::result::Result<Option<String>, ToolOutput> {
    match args.get("provider") {
        None => Err(ToolOutput::failed(
            ToolErrorCategory::InvalidArguments,
            "provider is required",
        )),
        Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => {
            let trimmed = value.trim();
            if trimmed.is_empty() || matches!(trimmed, "off" | "none") {
                Ok(None)
            } else {
                Ok(Some(trimmed.to_owned()))
            }
        }
        Some(_) => Err(ToolOutput::failed(
            ToolErrorCategory::InvalidArguments,
            "provider must be a string or null",
        )),
    }
}

/// Drop credential material, key fragments, and secret-bearing fields.
fn strip_secrets(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let stripped = map
                .into_iter()
                .filter(|(key, _)| !is_secret_field(key))
                .map(|(key, child)| (key, strip_secrets(child)))
                .collect();
            Value::Object(stripped)
        }
        Value::Array(items) => Value::Array(items.into_iter().map(strip_secrets).collect()),
        other => other,
    }
}

fn is_secret_field(name: &str) -> bool {
    let normalized = name
        .chars()
        .map(|character| {
            if character == '-' {
                '_'
            } else {
                character.to_ascii_lowercase()
            }
        })
        .collect::<String>();
    if matches!(
        normalized.as_str(),
        "has_api_key" | "has_credential" | "has_credentials"
    ) {
        return false;
    }
    matches!(
        normalized.as_str(),
        "api_key"
            | "apikey"
            | "credential"
            | "credentials"
            | "secret"
            | "secrets"
            | "token"
            | "access_token"
            | "refresh_token"
            | "id_token"
            | "password"
            | "passwd"
            | "authorization"
            | "private_key"
            | "privatekey"
            | "bearer"
            | "auth_token"
            | "client_secret"
    ) || normalized.ends_with("_api_key")
        || normalized.ends_with("_secret")
        || normalized.ends_with("_token")
        || normalized.ends_with("_password")
        || normalized.ends_with("_credential")
        || normalized.ends_with("_credentials")
}

fn is_image_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            IMAGE_EXTENSIONS
                .iter()
                .any(|candidate| extension.eq_ignore_ascii_case(candidate))
        })
}

// ---------------------------------------------------------------------------
// profile_snapshot
// ---------------------------------------------------------------------------

struct ProfileSnapshotTool {
    state: Arc<AgentMcp>,
}

fn profile_snapshot_spec() -> ToolSpec {
    ToolSpec {
        name: "profile_snapshot".into(),
        description: "One document of the profile axes a chat turn depends on: settings, providers (id/kind/has_credential only), the model catalog and roles, web-search config, and exec config. Never includes credentials."
            .into(),
        input_schema: object_schema(json!({}), &[]),
    }
}

#[async_trait::async_trait]
impl Tool for ProfileSnapshotTool {
    fn spec(&self) -> ToolSpec {
        profile_snapshot_spec()
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::ReadOnly
    }

    async fn execute(&self, _ctx: &ToolCtx, _args: Value) -> Result<ToolOutput> {
        let client = self.state.client.lock().await;
        let settings = match client.get_settings().await {
            Ok(settings) => strip_secrets(settings),
            Err(error) => return fail(error.to_string()),
        };
        let providers = match client.list_providers().await {
            Ok(providers) => providers
                .into_iter()
                .map(|provider| {
                    json!({
                        "id": provider.kind,
                        "kind": provider.kind,
                        "has_credential": provider.has_credential,
                    })
                })
                .collect::<Vec<Value>>(),
            Err(error) => return fail(error.to_string()),
        };
        let catalog = match client.list_models().await {
            Ok(catalog) => catalog,
            Err(error) => return fail(error.to_string()),
        };
        let models = catalog
            .models
            .iter()
            .map(|model| {
                json!({
                    "key": model.key,
                    "id": model.id,
                    "display_name": model.display_name,
                    "provider": model.provider,
                    "available": model.available,
                    "context_window": model.context_window,
                    "reasoning_efforts": model.reasoning_efforts,
                })
            })
            .collect::<Vec<Value>>();
        let roles = catalog
            .roles
            .iter()
            .map(|role| {
                json!({
                    "role": role.role,
                    "selection": role.selection,
                    "resolved_key": role.resolved_key,
                })
            })
            .collect::<Vec<Value>>();
        let web_search = match client.get_web_search_config().await {
            Ok(config) => strip_secrets(config),
            Err(error) => return fail(error.to_string()),
        };
        let exec = match client.get_code_execution_config().await {
            Ok(config) => strip_secrets(config),
            Err(error) => return fail(error.to_string()),
        };
        let data = json!({
            "settings": settings,
            "providers": providers,
            "models": models,
            "roles": roles,
            "web_search": web_search,
            "exec": exec,
        });
        Ok(ToolOutput::text("profile snapshot").with_data(data))
    }
}

// ---------------------------------------------------------------------------
// model_role_set
// ---------------------------------------------------------------------------

struct ModelRoleSetTool {
    state: Arc<AgentMcp>,
}

fn model_role_set_spec() -> ToolSpec {
    ToolSpec {
        name: "model_role_set".into(),
        description: "Pin a model role (chat or utility) to a catalog key, or pass auto to clear it back to automatic."
            .into(),
        input_schema: object_schema(
            json!({
                "role": {
                    "type": "string",
                    "enum": ["chat", "utility"],
                    "description": "Model role to pin.",
                },
                "model": {
                    "type": "string",
                    "description": "Catalog key, or auto to clear the pin.",
                },
            }),
            &["role", "model"],
        ),
    }
}

#[async_trait::async_trait]
impl Tool for ModelRoleSetTool {
    fn spec(&self) -> ToolSpec {
        model_role_set_spec()
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::Sensitive
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let Some(role) = args.get("role").and_then(Value::as_str) else {
            return fail_args("role is required");
        };
        if !MODEL_ROLES.contains(&role) {
            return fail_args("role must be chat or utility");
        }
        let Some(model) = args.get("model").and_then(Value::as_str) else {
            return fail_args("model is required");
        };
        let selection = match model.trim() {
            "" | "auto" | "automatic" => None,
            value => Some(value),
        };
        let client = self.state.client.lock().await;
        let info = match client.set_model_role(role, selection).await {
            Ok(info) => strip_secrets(info),
            Err(error) => return fail(error.to_string()),
        };
        Ok(ToolOutput::text(format!("role {role} updated")).with_data(info))
    }
}

// ---------------------------------------------------------------------------
// web_search_select
// ---------------------------------------------------------------------------

struct WebSearchSelectTool {
    state: Arc<AgentMcp>,
}

fn web_search_select_spec() -> ToolSpec {
    ToolSpec {
        name: "web_search_select".into(),
        description:
            "Select the host web-search provider. Pass off or null to turn host search off.".into(),
        input_schema: object_schema(
            json!({
                "provider": {
                    "type": ["string", "null"],
                    "description": "Web-search provider (exa, tavily, brave, firecrawl, searxng), or off/null to disable.",
                },
            }),
            &["provider"],
        ),
    }
}

#[async_trait::async_trait]
impl Tool for WebSearchSelectTool {
    fn spec(&self) -> ToolSpec {
        web_search_select_spec()
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::Sensitive
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let provider = match optional_provider(&args) {
            Ok(provider) => provider,
            Err(output) => return Ok(output),
        };
        let client = self.state.client.lock().await;
        let info = match client.set_web_search_provider(provider.as_deref()).await {
            Ok(info) => strip_secrets(info),
            Err(error) => return fail(error.to_string()),
        };
        let text = match provider.as_deref() {
            Some(provider) => format!("web search now uses {provider}"),
            None => "host web search is off".to_owned(),
        };
        Ok(ToolOutput::text(text).with_data(info))
    }
}

// ---------------------------------------------------------------------------
// exec_select
// ---------------------------------------------------------------------------

struct ExecSelectTool {
    state: Arc<AgentMcp>,
}

fn exec_select_spec() -> ToolSpec {
    ToolSpec {
        name: "exec_select".into(),
        description: "Select the code-execution backend. Pass off or null to disable execution."
            .into(),
        input_schema: object_schema(
            json!({
                "provider": {
                    "type": ["string", "null"],
                    "description": "Execution provider (local, e2b, docker, daytona), or off/null to disable.",
                },
            }),
            &["provider"],
        ),
    }
}

#[async_trait::async_trait]
impl Tool for ExecSelectTool {
    fn spec(&self) -> ToolSpec {
        exec_select_spec()
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::Sensitive
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let provider = match optional_provider(&args) {
            Ok(provider) => provider,
            Err(output) => return Ok(output),
        };
        let client = self.state.client.lock().await;
        let info = match client
            .set_code_execution_provider(provider.as_deref())
            .await
        {
            Ok(info) => strip_secrets(info),
            Err(error) => return fail(error.to_string()),
        };
        let text = match provider.as_deref() {
            Some(provider) => format!("code execution now uses {provider}"),
            None => "code execution is off".to_owned(),
        };
        Ok(ToolOutput::text(text).with_data(info))
    }
}

// ---------------------------------------------------------------------------
// chat_set_model
// ---------------------------------------------------------------------------

struct ChatSetModelTool {
    state: Arc<AgentMcp>,
}

fn chat_set_model_spec() -> ToolSpec {
    ToolSpec {
        name: "chat_set_model".into(),
        description:
            "Pin a catalog model on one chat. Omit model or pass null to clear the override.".into(),
        input_schema: object_schema(
            json!({
                "chat_id": chat_id_property(),
                "model": {
                    "type": ["string", "null"],
                    "description": "Catalog key to pin, or null to clear the override.",
                },
            }),
            &["chat_id"],
        ),
    }
}

#[async_trait::async_trait]
impl Tool for ChatSetModelTool {
    fn spec(&self) -> ToolSpec {
        chat_set_model_spec()
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::Sensitive
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let chat = match required_chat_id(&args) {
            Ok(chat) => chat,
            Err(output) => return Ok(output),
        };
        let model = match args.get("model") {
            None | Some(Value::Null) => None,
            Some(Value::String(value)) => {
                let trimmed = value.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed)
                }
            }
            Some(_) => return fail_args("model must be a string or null"),
        };
        let client = self.state.client.lock().await;
        if let Err(error) = client.set_chat_model(chat, model).await {
            return fail(error.to_string());
        }
        let data = json!({
            "chat_id": chat,
            "model": model,
        });
        let text = match model {
            Some(model) => format!("chat {chat} model {model}"),
            None => format!("chat {chat} model cleared"),
        };
        Ok(ToolOutput::text(text).with_data(data))
    }
}

// ---------------------------------------------------------------------------
// chat_set_permission_mode
// ---------------------------------------------------------------------------

struct ChatSetPermissionModeTool {
    state: Arc<AgentMcp>,
}

fn chat_set_permission_mode_spec() -> ToolSpec {
    ToolSpec {
        name: "chat_set_permission_mode".into(),
        description: "Set a chat's permission mode to plan, ask, auto, or allow.".into(),
        input_schema: object_schema(
            json!({
                "chat_id": chat_id_property(),
                "permission_mode": {
                    "type": "string",
                    "enum": ["plan", "ask", "auto", "allow"],
                    "description": "Permission mode stored on the chat.",
                },
            }),
            &["chat_id", "permission_mode"],
        ),
    }
}

#[async_trait::async_trait]
impl Tool for ChatSetPermissionModeTool {
    fn spec(&self) -> ToolSpec {
        chat_set_permission_mode_spec()
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::Sensitive
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let chat = match required_chat_id(&args) {
            Ok(chat) => chat,
            Err(output) => return Ok(output),
        };
        let Some(mode) = args.get("permission_mode").and_then(Value::as_str) else {
            return fail_args("permission_mode is required");
        };
        if !PERMISSION_MODES.contains(&mode) {
            return fail_args("permission_mode must be plan, ask, auto, or allow");
        }
        let client = self.state.client.lock().await;
        let summary = match client.set_chat_permission_mode(chat, Some(mode)).await {
            Ok(summary) => summary,
            Err(error) => return fail(error.to_string()),
        };
        let data = json!({
            "chat_id": chat,
            "permission_mode": summary.permission_mode,
        });
        Ok(ToolOutput::text(format!("permission_mode: {mode}")).with_data(data))
    }
}

// ---------------------------------------------------------------------------
// chat_attach_file
// ---------------------------------------------------------------------------

struct ChatAttachFileTool {
    state: Arc<AgentMcp>,
}

fn chat_attach_file_spec() -> ToolSpec {
    ToolSpec {
        name: "chat_attach_file".into(),
        description: "Attach a local file to a chat. Image extensions go through the image route; everything else is ingested as a document."
            .into(),
        input_schema: object_schema(
            json!({
                "chat_id": chat_id_property(),
                "path": {
                    "type": "string",
                    "description": "Local filesystem path to attach.",
                },
            }),
            &["chat_id", "path"],
        ),
    }
}

#[async_trait::async_trait]
impl Tool for ChatAttachFileTool {
    fn spec(&self) -> ToolSpec {
        chat_attach_file_spec()
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::Sensitive
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let chat = match required_chat_id(&args) {
            Ok(chat) => chat,
            Err(output) => return Ok(output),
        };
        let Some(path) = args.get("path").and_then(Value::as_str) else {
            return fail_args("path is required");
        };
        if path.trim().is_empty() {
            return fail_args("path must not be empty");
        }
        let path = Path::new(path);
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) => {
                return fail(format!("could not read {}: {error}", path.display()));
            }
        };
        if bytes.is_empty() {
            return fail(format!("{} is empty", path.display()));
        }
        let media_type = tidebreak_server::media_type::sniff_media_type_for_path(&bytes, path);
        let title = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("attachment");
        let client = self.state.client.lock().await;
        if is_image_path(path) {
            let id = match client.attach_image(chat, &media_type, bytes).await {
                Ok(id) => id,
                Err(error) => return fail(error.to_string()),
            };
            let data = json!({
                "id": id,
                "kind": "image",
            });
            return Ok(ToolOutput::text(format!("attached image {id}")).with_data(data));
        }
        let id = match client
            .attach_document(chat, title, &media_type, bytes)
            .await
        {
            Ok(id) => id,
            Err(error) => return fail(error.to_string()),
        };
        let data = json!({
            "id": id,
            "kind": "document",
        });
        Ok(ToolOutput::text(format!("attached document {id}")).with_data(data))
    }
}

// ---------------------------------------------------------------------------
// chat_outputs
// ---------------------------------------------------------------------------

struct ChatOutputsTool {
    state: Arc<AgentMcp>,
}

fn chat_outputs_spec() -> ToolSpec {
    ToolSpec {
        name: "chat_outputs".into(),
        description: "List the conversation's live outputs.".into(),
        input_schema: object_schema(json!({ "chat_id": chat_id_property() }), &["chat_id"]),
    }
}

#[async_trait::async_trait]
impl Tool for ChatOutputsTool {
    fn spec(&self) -> ToolSpec {
        chat_outputs_spec()
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::ReadOnly
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let chat = match required_chat_id(&args) {
            Ok(chat) => chat,
            Err(output) => return Ok(output),
        };
        let client = self.state.client.lock().await;
        let catalog = match client.list_outputs(chat).await {
            Ok(catalog) => catalog,
            Err(error) => return fail(error.to_string()),
        };
        let outputs = catalog
            .deliverables
            .iter()
            .map(|output| {
                json!({
                    "output_id": output.output_id,
                    "filename": output.filename,
                    "media_type": output.media_type,
                    "size_bytes": output.size_bytes,
                    "revision_count": output.revision_count,
                    "updated_at": output.updated_at,
                })
            })
            .collect::<Vec<Value>>();
        let data = json!({
            "outputs": outputs,
            "truncated": catalog.truncated,
        });
        Ok(ToolOutput::text(format!("{} output(s)", outputs.len())).with_data(data))
    }
}

// ---------------------------------------------------------------------------
// chat_output_read
// ---------------------------------------------------------------------------

struct ChatOutputReadTool {
    state: Arc<AgentMcp>,
}

fn chat_output_read_spec() -> ToolSpec {
    ToolSpec {
        name: "chat_output_read".into(),
        description:
            "Read one output. Text content is returned as text; binary outputs return base64 bytes."
                .into(),
        input_schema: object_schema(
            json!({
                "chat_id": chat_id_property(),
                "output": {
                    "type": "string",
                    "description": "Output UUID.",
                },
                "revision": {
                    "type": "string",
                    "description": "Optional revision UUID. Omit for the current revision.",
                },
            }),
            &["chat_id", "output"],
        ),
    }
}

#[async_trait::async_trait]
impl Tool for ChatOutputReadTool {
    fn spec(&self) -> ToolSpec {
        chat_output_read_spec()
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::ReadOnly
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let chat = match required_chat_id(&args) {
            Ok(chat) => chat,
            Err(output) => return Ok(output),
        };
        let Some(output) = args.get("output").and_then(Value::as_str) else {
            return fail_args("output is required");
        };
        let output = match OutputId::from_str(output) {
            Ok(output) => output,
            Err(_) => return fail_args("output must be a UUID"),
        };
        let revision = match args.get("revision") {
            None | Some(Value::Null) => None,
            Some(value) => {
                let Some(revision) = value.as_str() else {
                    return fail_args("revision must be a UUID");
                };
                match OutputRevisionId::from_str(revision) {
                    Ok(revision) => Some(revision),
                    Err(_) => return fail_args("revision must be a UUID"),
                }
            }
        };
        let client = self.state.client.lock().await;
        let preview = match client.read_output(chat, output, revision).await {
            Ok(preview) => preview,
            Err(error) => return fail(error.to_string()),
        };
        if media_type_is_text(&preview.media_type) || !preview.content.is_empty() {
            let data = json!({
                "output_id": output,
                "filename": preview.filename,
                "media_type": preview.media_type,
                "revision_id": preview.revision_id,
                "content": preview.content,
                "truncated": preview.truncated,
            });
            return Ok(ToolOutput::text(preview.content).with_data(data));
        }
        let bytes = match client.read_output_bytes(chat, output, revision).await {
            Ok(bytes) => bytes,
            Err(error) => return fail(error.to_string()),
        };
        let data = json!({
            "output_id": output,
            "filename": preview.filename,
            "media_type": preview.media_type,
            "revision_id": preview.revision_id,
            "bytes": BASE64.encode(&bytes),
            "encoding": "base64",
        });
        Ok(ToolOutput::text(format!(
            "{} ({} bytes, base64)",
            preview.filename,
            bytes.len()
        ))
        .with_data(data))
    }
}

// ---------------------------------------------------------------------------
// agent_runs
// ---------------------------------------------------------------------------

struct AgentRunsTool {
    state: Arc<AgentMcp>,
}

fn agent_runs_spec() -> ToolSpec {
    ToolSpec {
        name: "agent_runs".into(),
        description: "List background agent runs for a chat. Activity timelines are omitted; they are a per-run fetch."
            .into(),
        input_schema: object_schema(json!({ "chat_id": chat_id_property() }), &["chat_id"]),
    }
}

#[async_trait::async_trait]
impl Tool for AgentRunsTool {
    fn spec(&self) -> ToolSpec {
        agent_runs_spec()
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::ReadOnly
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let chat = match required_chat_id(&args) {
            Ok(chat) => chat,
            Err(output) => return Ok(output),
        };
        let client = self.state.client.lock().await;
        let runs = match client.list_agent_runs(chat).await {
            Ok(runs) => runs,
            Err(error) => return fail(error.to_string()),
        };
        let summaries = runs
            .iter()
            .map(|run| {
                json!({
                    "id": run.id,
                    "parent_id": run.parent_id,
                    "tier": run.tier,
                    "execution_location": run.execution_location,
                    "code_execution_provider": run.code_execution_provider,
                    "status": run.status,
                    "task": run.task,
                    "started_at": run.started_at,
                    "finished_at": run.finished_at,
                    "last_error_code": run.last_error_code,
                    "submitted_outputs": run.submitted_outputs.iter().map(|output| {
                        json!({
                            "output_id": output.output_id,
                            "filename": output.filename,
                        })
                    }).collect::<Vec<Value>>(),
                    "terminal_text": run.terminal_text,
                    "spawn_call_id": run.spawn_call_id,
                    "created_at": run.created_at,
                })
            })
            .collect::<Vec<Value>>();
        let data = json!({ "runs": summaries });
        Ok(ToolOutput::text(format!("{} run(s)", summaries.len())).with_data(data))
    }
}

// ---------------------------------------------------------------------------
// agent_run_cancel
// ---------------------------------------------------------------------------

struct AgentRunCancelTool {
    state: Arc<AgentMcp>,
}

fn agent_run_cancel_spec() -> ToolSpec {
    ToolSpec {
        name: "agent_run_cancel".into(),
        description: "Ask a background agent run to stop.".into(),
        input_schema: object_schema(
            json!({
                "chat_id": chat_id_property(),
                "run_id": {
                    "type": "string",
                    "description": "Background run UUID.",
                },
            }),
            &["chat_id", "run_id"],
        ),
    }
}

#[async_trait::async_trait]
impl Tool for AgentRunCancelTool {
    fn spec(&self) -> ToolSpec {
        agent_run_cancel_spec()
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::Sensitive
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let chat = match required_chat_id(&args) {
            Ok(chat) => chat,
            Err(output) => return Ok(output),
        };
        let Some(run_id) = args.get("run_id").and_then(Value::as_str) else {
            return fail_args("run_id is required");
        };
        let run_id = match AgentRunId::from_str(run_id) {
            Ok(run_id) => run_id,
            Err(_) => return fail_args("run_id must be a UUID"),
        };
        let client = self.state.client.lock().await;
        if let Err(error) = client.cancel_agent_run(chat, run_id).await {
            return fail(error.to_string());
        }
        Ok(ToolOutput::text("cancelled").with_data(json!({
            "ok": true,
            "chat_id": chat,
            "run_id": run_id,
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_secrets_drops_credential_fields_and_keeps_presence_flags() {
        let dirty = json!({
            "has_api_key": true,
            "has_credential": false,
            "api_key": "sk-secret",
            "credential": {"type": "api_key", "key": "sk-secret"},
            "token": "abc",
            "nested": {
                "client_secret": "nope",
                "name": "ok",
            },
        });
        let clean = strip_secrets(dirty);
        assert_eq!(clean["has_api_key"], true);
        assert_eq!(clean["has_credential"], false);
        assert!(clean.get("api_key").is_none());
        assert!(clean.get("credential").is_none());
        assert!(clean.get("token").is_none());
        assert!(clean["nested"].get("client_secret").is_none());
        assert_eq!(clean["nested"]["name"], "ok");
    }
}
