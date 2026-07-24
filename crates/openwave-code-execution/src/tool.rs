use std::sync::Arc;

use async_trait::async_trait;
use openwave_core::{ApprovalClass, Result, Tool, ToolCtx, ToolOutput, ToolSpec};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    CodeExecutionProvider, CodeExecutionRequest, ExecutionId, ExecutionWorkspaceId, MAX_ARGUMENTS,
    MAX_COMMAND_BYTES, MAX_CWD_BYTES,
};

pub const EXEC_TOOL_NAME: &str = "exec";

/// Model-facing command execution backed by a host-selected provider.
pub struct ExecTool {
    provider: Arc<dyn CodeExecutionProvider>,
}

impl ExecTool {
    #[must_use]
    pub fn new(provider: Arc<dyn CodeExecutionProvider>) -> Self {
        Self { provider }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecArguments {
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default = "default_cwd")]
    cwd: String,
}

fn default_cwd() -> String {
    ".".into()
}

#[async_trait]
impl Tool for ExecTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: EXEC_TOOL_NAME.into(),
            description: "Run one executable with an argument vector in the configured isolated \
                          execution provider. No shell parses the arguments unless you explicitly \
                          invoke a shell. The local provider blocks network and user-data access \
                          outside private chat scratch, confines all writes there, and returns \
                          bounded stdout/stderr."
                .into(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "command": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": MAX_COMMAND_BYTES,
                        "description": "Executable name or path."
                    },
                    "args": {
                        "type": "array",
                        "maxItems": MAX_ARGUMENTS,
                        "items": { "type": "string" },
                        "description": "Arguments passed directly to the executable."
                    },
                    "cwd": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": MAX_CWD_BYTES,
                        "description": "Private-scratch-relative working directory (defaults to '.')."
                    }
                },
                "required": ["command"]
            }),
        }
    }

    fn approval_class(&self) -> ApprovalClass {
        // Command execution remains an explicit consent boundary even though
        // the local provider denies network and outside-workspace writes.
        ApprovalClass::Sensitive
    }

    async fn execute(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let arguments: ExecArguments = match serde_json::from_value(args) {
            Ok(arguments) => arguments,
            Err(error) => {
                return Ok(ToolOutput::error(format!("invalid arguments: {error}")));
            }
        };
        let Some(call_id) = ctx.call_id else {
            return Ok(ToolOutput::error(
                "code execution requires a stable tool-call identity",
            ));
        };
        let execution_id = match ExecutionId::parse(call_id.to_string()) {
            Ok(id) => id,
            Err(error) => return Ok(ToolOutput::error(error.to_string())),
        };
        let workspace_id = match ExecutionWorkspaceId::parse(ctx.chat_id.to_string()) {
            Ok(id) => id,
            Err(error) => return Ok(ToolOutput::error(error.to_string())),
        };
        let request = match CodeExecutionRequest::new(
            execution_id,
            workspace_id,
            arguments.command,
            arguments.args,
            arguments.cwd,
        ) {
            Ok(request) => request,
            Err(error) => return Ok(ToolOutput::error(error.to_string())),
        };
        let response = match self.provider.execute(request).await {
            Ok(response) => response,
            Err(error) => return Ok(ToolOutput::error(error.to_string())),
        };

        let exit = response
            .exit_code
            .map_or_else(|| "signal".into(), |code| code.to_string());
        let mut content = format!(
            "provider: {}\nexit: {exit}\nduration_ms: {}",
            response.provider, response.duration_ms
        );
        if response.timed_out {
            content.push_str("\ntimed_out: true");
        }
        if response.output_truncated {
            content.push_str("\noutput_truncated: true");
        }
        if !response.stdout.is_empty() {
            content.push_str("\n\nstdout:\n");
            content.push_str(&response.stdout);
        }
        if !response.stderr.is_empty() {
            content.push_str("\n\nstderr:\n");
            content.push_str(&response.stderr);
        }
        let failed = response.timed_out || response.exit_code != Some(0);
        // `stdout`/`stderr` ride here as well as in the model-facing content
        // so the renderer's closed result projection can read them field by
        // field rather than parsing them back out of prose.
        let mut output = ToolOutput::text(content).with_data(json!({
            "provider": response.provider,
            "exit_code": response.exit_code,
            "timed_out": response.timed_out,
            "output_truncated": response.output_truncated,
            "duration_ms": response.duration_ms,
            "stdout": response.stdout,
            "stderr": response.stderr,
        }));
        output.is_error = failed;
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CodeExecutionError, CodeExecutionProviderKind, CodeExecutionResponse};
    use openwave_core::{CallId, ChatId};
    use std::path::PathBuf;
    use std::sync::Mutex;

    #[derive(Default)]
    struct RecordingProvider {
        request: Mutex<Option<CodeExecutionRequest>>,
    }

    #[async_trait]
    impl CodeExecutionProvider for RecordingProvider {
        async fn execute(
            &self,
            request: CodeExecutionRequest,
        ) -> std::result::Result<CodeExecutionResponse, CodeExecutionError> {
            *self.request.lock().unwrap() = Some(request);
            Ok(CodeExecutionResponse {
                provider: CodeExecutionProviderKind::Local,
                exit_code: Some(0),
                stdout: "ok\n".into(),
                stderr: String::new(),
                timed_out: false,
                output_truncated: false,
                duration_ms: 3,
            })
        }
    }

    #[tokio::test]
    async fn tool_passes_stable_host_identities_and_structured_arguments() {
        let provider = Arc::new(RecordingProvider::default());
        let tool = ExecTool::new(provider.clone());
        let chat_id = ChatId::new();
        let call_id = CallId::new();
        let ctx = ToolCtx::new_legacy_workspace(chat_id, None, PathBuf::from("/tmp/unused"))
            .with_call_id(call_id);

        let output = tool
            .execute(
                &ctx,
                json!({"command": "/bin/echo", "args": ["ok"], "cwd": "."}),
            )
            .await
            .unwrap();

        assert!(!output.is_error);
        assert!(output.content.contains("stdout:\nok"));
        let request = provider.request.lock().unwrap().clone().unwrap();
        assert_eq!(request.execution_id.as_str(), call_id.to_string());
        assert_eq!(request.workspace_id.as_str(), chat_id.to_string());
        assert_eq!(request.command, "/bin/echo");
    }

    #[test]
    fn exec_is_sensitive_and_has_a_closed_schema() {
        let tool = ExecTool::new(Arc::new(RecordingProvider::default()));
        assert_eq!(tool.approval_class(), ApprovalClass::Sensitive);
        let spec = tool.spec();
        assert_eq!(spec.name, EXEC_TOOL_NAME);
        assert_eq!(spec.input_schema["additionalProperties"], false);
    }
}
