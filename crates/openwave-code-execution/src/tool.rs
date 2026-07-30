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
                          outside private chat scratch; managed cloud sandboxes (E2B, Daytona) \
                          have the chat's private scratch mirrored in before the command runs and \
                          mirrored back after, so files from write_file are visible here and \
                          files this command writes are visible to read_file. Every provider \
                          returns bounded stdout/stderr. For visual review, save up to three \
                          PNG, JPEG, or WebP images in preview/; overview, grid, thumbnail, page, \
                          and slide filenames are prioritized. Use output/ for durable artifacts. \
                          When bundled document helpers are present, invoke them directly from \
                          .openwave/exec-scripts. Examples: command python3 with args \
                          [\".openwave/exec-scripts/render_pdf.py\", \"documents/report.pdf\", \
                          \"--pages\", \"1-2\"]; command python3 with args \
                          [\".openwave/exec-scripts/extract_pdf_figures.py\", \
                          \"documents/report.pdf\"]; or command python3 with args \
                          [\".openwave/exec-scripts/analyze_xlsx.py\", \
                          \"documents/model.xlsx\"]. Each helper writes visual review files to \
                          preview/ and prints a concise summary; a missing Python or document \
                          dependency is reported as a command error."
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
            workspace_id.clone(),
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
        let successful = !response.timed_out && response.exit_code == Some(0);
        let previews = if successful {
            match self.provider.collect_preview_images(&workspace_id).await {
                Ok(previews) => previews,
                Err(error) => crate::PreviewScan {
                    images: Vec::new(),
                    notes: vec![format!("preview images unavailable: {error}")],
                },
            }
        } else {
            crate::PreviewScan::default()
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
        if !response.sync_notes.is_empty() {
            content.push_str("\n\nworkspace sync:");
            for note in &response.sync_notes {
                content.push('\n');
                content.push_str(note);
            }
        }
        if !previews.notes.is_empty() {
            content.push_str("\n\npreview scan:");
            for note in &previews.notes {
                content.push('\n');
                content.push_str(note);
            }
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
        let mut output = ToolOutput::text(content)
            .with_data(json!({
                "provider": response.provider,
                "exit_code": response.exit_code,
                "timed_out": response.timed_out,
                "output_truncated": response.output_truncated,
                "duration_ms": response.duration_ms,
                "stdout": response.stdout,
                "stderr": response.stderr,
                "sync_notes": response.sync_notes,
            }))
            .with_images(previews.images);
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
    use std::sync::atomic::{AtomicUsize, Ordering};
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
                sync_notes: Vec::new(),
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
        assert!(spec.description.contains("preview/"));
        assert!(spec.description.contains(".openwave/exec-scripts"));
        assert!(spec.description.contains("render_pdf.py"));
        assert!(spec.description.contains("analyze_xlsx.py"));
    }

    struct PreviewProvider {
        exit_code: i32,
        scans: AtomicUsize,
    }

    #[async_trait]
    impl CodeExecutionProvider for PreviewProvider {
        async fn execute(
            &self,
            _request: CodeExecutionRequest,
        ) -> std::result::Result<CodeExecutionResponse, CodeExecutionError> {
            Ok(CodeExecutionResponse {
                provider: CodeExecutionProviderKind::Local,
                exit_code: Some(self.exit_code),
                stdout: String::new(),
                stderr: String::new(),
                timed_out: false,
                output_truncated: false,
                duration_ms: 1,
                sync_notes: Vec::new(),
            })
        }

        async fn collect_preview_images(
            &self,
            _workspace: &ExecutionWorkspaceId,
        ) -> std::result::Result<crate::PreviewScan, CodeExecutionError> {
            self.scans.fetch_add(1, Ordering::SeqCst);
            let bytes = vec![1, 2, 3];
            let image = openwave_core::ImageRef {
                blob_id: openwave_core::DocumentSourceBlob::from_bytes(&bytes).id,
                media_type: openwave_core::ImageMediaType::Png,
                width: 1,
                height: 1,
                byte_len: 3,
            };
            Ok(crate::PreviewScan {
                images: vec![(
                    image,
                    openwave_core::ImageData::new(image.media_type, bytes),
                )],
                notes: Vec::new(),
            })
        }
    }

    #[tokio::test]
    async fn previews_are_collected_only_after_success() {
        for (exit_code, expected_scans, expected_images) in [(0, 1, 1), (2, 0, 0)] {
            let provider = Arc::new(PreviewProvider {
                exit_code,
                scans: AtomicUsize::new(0),
            });
            let tool = ExecTool::new(provider.clone());
            let output = tool
                .execute(
                    &ToolCtx::new_legacy_workspace(
                        ChatId::new(),
                        None,
                        PathBuf::from("/tmp/unused"),
                    )
                    .with_call_id(CallId::new()),
                    json!({"command": "/bin/true"}),
                )
                .await
                .unwrap();
            assert_eq!(provider.scans.load(Ordering::SeqCst), expected_scans);
            assert_eq!(output.images.len(), expected_images);
        }
    }
}
